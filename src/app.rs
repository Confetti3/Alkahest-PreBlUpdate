#![warn(rust_2018_idioms)]
#![deny(clippy::correctness, clippy::suspicious, clippy::complexity)]
#![allow(clippy::collapsible_else_if, clippy::missing_transmute_annotations)]

use std::{
    collections::hash_map::Entry,
    io::{Cursor, Seek},
    rc::Rc,
    str::FromStr,
    sync::Arc,
    time::{Duration, Instant},
};

use ahash::HashMap;
use alkahest_core::{config_relative_path, job::SCHEDULER};
use alkahest_data::{
    hash::fnv1,
    strings::{StringContainer, StringContainerShared},
    tag::WideHash,
};
use alkahest_render::{
    Gpu, Renderer,
    gpu::{AdapterPreference as GpuAdapterPreference, command_list::CommandList},
    util::fps_histogram::FrametimeHistogram,
};
use anyhow::Context;
use parking_lot::RwLock;
use sdl3::video::Window;
use tiger_parse::TigerReadable;
use tiger_pkg::{TagHash, package_manager};

use crate::{
    cli::AppArgs,
    config::AppConfig,
    task::Task,
    ui::{
        Gui,
        tabs::{Tab, inspector::InspectorTab},
    },
};

pub struct App {
    pub sdl: Rc<sdl3::Sdl>,
    pub window: Rc<Window>,
    pub _gpu: Arc<Gpu>,
    pub renderer: Option<Arc<Renderer>>,
    pub gui: Gui,
    pub running: bool,

    shared_state: Arc<SharedState>,

    renderer_task: Option<
        Task<anyhow::Result<alkahest_render::renderer::shadowkeep::ShadowkeepRendererBootstrap>>,
    >,
    pub renderer_status: RendererStatus,
    last_frame_time: Instant,
    frametime_histogram: FrametimeHistogram,
}

#[derive(Debug, Clone)]
pub enum RendererStatus {
    Initializing,
    Ready,
    Disabled,
    Blocked(String),
    Failed(String),
}

impl RendererStatus {
    pub fn message(&self) -> &str {
        match self {
            Self::Initializing => "Initializing the Shadowkeep renderer in the background…",
            Self::Ready => "Shadowkeep renderer ready",
            Self::Disabled => "3D renderer disabled with --no-3d",
            Self::Blocked(_) => {
                "Shadowkeep core geometry is blocked; catalog and inspection remain active"
            }
            Self::Failed(_) => {
                "Shadowkeep renderer is unavailable; catalog and inspection remain active"
            }
        }
    }

    /// A user-facing explanation suitable for an action that would otherwise
    /// dereference the global renderer singleton.  UI code must use this
    /// rather than treating a background renderer task as completed.
    pub fn scene_diagnostic(&self) -> String {
        match self {
            Self::Initializing => "3D scene initialization is still in progress. Map metadata \
                                   remains available; retry the rendered view after the renderer \
                                   reports ready."
                .to_string(),
            Self::Ready => "The Shadowkeep renderer is ready. Maps may begin loading their \
                            decoded scene and GPU assets."
                .to_string(),
            Self::Disabled => "3D rendering was disabled with --no-3d. Map metadata remains \
                               available."
                .to_string(),
            Self::Blocked(detail) => format!(
                "Shadowkeep core geometry is blocked. Map metadata remains available. Exact \
                 diagnostic: {detail}"
            ),
            Self::Failed(detail) => format!(
                "The Shadowkeep renderer is unavailable. Map metadata remains available. Exact \
                 initialization diagnostic: {detail}"
            ),
        }
    }

    pub fn is_ready(&self) -> bool {
        matches!(self, Self::Ready)
    }
}

impl App {
    pub fn new(sdl: Rc<sdl3::Sdl>, window: Rc<Window>, args: AppArgs) -> anyhow::Result<Self> {
        #[cfg(feature = "wwise")]
        crate::audio::init_sound_engine().context("Failed to initialize sound engine")?;

        let gpu_preference = match args.adapter {
            crate::cli::AdapterPreference::Auto => GpuAdapterPreference::Auto,
            crate::cli::AdapterPreference::Hardware => GpuAdapterPreference::Hardware,
            crate::cli::AdapterPreference::Warp => GpuAdapterPreference::Warp,
        };
        let gpu = Arc::new(
            Gpu::create_with_adapter_preference(&window, gpu_preference)
                .context("Failed to create GPU platform")?,
        );
        info!(adapter = %gpu.get_adapter_name(), ?gpu_preference, "GPU platform initialized");
        let shared_state: Arc<SharedState> = SharedState::new()
            .context("Failed to create shared state")?
            .into();
        crate::world::shadowkeep_map::initialize_shadowkeep_bubble_catalog(|hash| {
            shared_state.wordlist.get(&hash).cloned()
        })
        .context("Failed to initialize Shadowkeep bubble catalog")?;
        let mut gui = Gui::new(&gpu, sdl.clone(), window.clone())?;
        if let Some(tag_hash) = args.open_map.as_ref() {
            match TagHash::from_str(tag_hash) {
                Ok(tag) => match crate::ui::tabs::map::MapTab::new(
                    tag,
                    format!("map {tag}"),
                    &shared_state,
                ) {
                    Ok(tab) => gui.add_tab(Tab::Map(tab)),
                    Err(error) => error!("Failed to open Shadowkeep map {tag}: {error:#}"),
                },
                Err(error) => error!("Failed to parse --open-map tag hash {tag_hash}: {error:?}"),
            }
        }

        if let Some(tag_hash) = args.open_tag.as_ref() {
            match TagHash::from_str(tag_hash) {
                Ok(tag) => gui.add_tab(Tab::Inspector(InspectorTab::new(
                    tag,
                    crate::inspection::InspectionKind::Tag,
                ))),
                Err(error) => error!("Failed to parse tag hash {tag_hash}: {error:?}"),
            }
        }

        if let Some(tag_hash) = args.open_activity.as_ref() {
            match TagHash::from_str(tag_hash) {
                Ok(tag) => gui.add_tab(Tab::Inspector(InspectorTab::new(
                    tag,
                    crate::inspection::InspectionKind::Activity,
                ))),
                Err(error) => error!("Failed to parse activity hash {tag_hash}: {error:?}"),
            }
        }

        if args.test_scene {
            warn!("--test-scene is queued until the Shadowkeep renderer is ready");
        }

        let (renderer_task, renderer_status) = if args.no_3d {
            (None, RendererStatus::Disabled)
        } else {
            let renderer_gpu = gpu.clone();
            let renderer_shared_state = shared_state.clone();
            (
                Some(Task::new("shadowkeep_renderer".to_string(), move || {
                    // The era bootstrap remains renderer-owned and is passed
                    // into the real constructor; it never falls back to the
                    // post-BL named render globals.
                    let bootstrap =
                        alkahest_render::renderer::shadowkeep::ShadowkeepRendererBootstrap::load(
                            renderer_gpu.clone(),
                        )
                        .context("Failed to construct Shadowkeep renderer bootstrap")?;
                    *renderer_shared_state.renderer_capabilities.write() =
                        bootstrap.capability_ledger();
                    Ok(bootstrap)
                })),
                RendererStatus::Initializing,
            )
        };
        *shared_state.renderer_status.write() = renderer_status.clone();

        Ok(Self {
            renderer: None,
            gui,
            sdl,
            window,
            _gpu: gpu,
            renderer_task,
            renderer_status,
            running: true,

            shared_state,

            last_frame_time: Instant::now(),
            frametime_histogram: FrametimeHistogram::new(10),
        })
    }

    pub fn handle_event(&mut self, event: sdl3::event::Event) {
        #[allow(clippy::single_match, clippy::collapsible_match)]
        match &event {
            sdl3::event::Event::Quit { .. } => {
                self.running = false;
            }
            sdl3::event::Event::Window { win_event, .. } => match win_event {
                &sdl3::event::WindowEvent::Resized(new_width, new_height) => {
                    self.gui
                        .egui_d3d11
                        .resize_buffers(&self._gpu, || {
                            self._gpu
                                .resize_swapchain((new_width as u32, new_height as u32));
                            Ok(())
                        })
                        .ok();
                }
                sdl3::event::WindowEvent::CloseRequested => {
                    self.running = false;
                }
                _ => {}
            },
            _ => {}
        };

        self.gui
            .egui_sdl3
            .handle_event(&event, &self.sdl, &self.sdl.video().unwrap());
    }

    #[profiling::function]
    pub fn render(&mut self, _event_pump: &sdl3::EventPump) {
        let delta_time = self.last_frame_time.elapsed().as_secs_f32();
        self.last_frame_time = std::time::Instant::now();

        self.frametime_histogram.push(delta_time);

        self.poll_renderer_task();

        if let Some(renderer) = self.renderer.as_ref() {
            renderer.begin_frame();
        }

        let gpu = &self._gpu;
        let mut cmd = CommandList::from_device_context(gpu, gpu.context().clone());
        subsecond::call(|| {
            self.gui.draw(&mut cmd, &self.shared_state);
        });

        gpu.present(self.shared_state.config.read().vsync);

        let config = self.shared_state.config.read();
        if config.framelimiter_enabled {
            let target_frame_delta = 1.0 / config.framerate_limit as f32;
            while self.last_frame_time.elapsed().as_secs_f32() < target_frame_delta {
                std::hint::spin_loop();
            }
        }

        if !self.window.has_input_focus() && !self.window.has_mouse_focus() {
            std::thread::sleep(Duration::from_millis(50));
        }

        #[cfg(feature = "wwise")]
        rrise::sound_engine::render_audio(true);

        profiling::finish_frame!();
    }
}

impl App {
    fn poll_renderer_task(&mut self) {
        let Some(task) = self.renderer_task.as_mut() else {
            return;
        };
        let Some(result) = task.get() else {
            return;
        };
        self.renderer_task = None;

        match result {
            Ok(Ok(bootstrap)) => {
                // `ThreadMutCell` intentionally pins renderer mutable state to
                // the UI/render thread. Bootstrap parsing is background-safe,
                // but the renderer must be finalized here before a Scene can
                // submit its first frame.
                match Renderer::new_shadowkeep(self._gpu.clone(), bootstrap) {
                    Ok(renderer) => {
                        let renderer = Arc::new(renderer);
                        Renderer::set_instance(renderer.clone());
                        self.renderer = Some(renderer);
                        self.renderer_status = RendererStatus::Ready;
                        *self.shared_state.renderer_status.write() = self.renderer_status.clone();
                        info!("Shadowkeep renderer initialized successfully");
                    }
                    Err(error) => {
                        error!("Shadowkeep renderer unavailable: {error:#}");
                        self.renderer_status = RendererStatus::Failed(format!("{error:#}"));
                        *self.shared_state.renderer_status.write() = self.renderer_status.clone();
                    }
                }
            }
            Ok(Err(error)) => {
                error!("Shadowkeep renderer unavailable: {error:#}");
                self.renderer_status = RendererStatus::Failed(format!("{error:#}"));
                *self.shared_state.renderer_status.write() = self.renderer_status.clone();
            }
            Err(_) => {
                self.renderer_status = RendererStatus::Failed(
                    "renderer initialization panicked; see the isolated panic log".to_string(),
                );
                *self.shared_state.renderer_status.write() = self.renderer_status.clone();
            }
        }
    }
}

impl Drop for App {
    fn drop(&mut self) {
        self.shared_state.save_config().ok();
        #[cfg(feature = "wwise")]
        if let Err(e) = crate::audio::term_sound_engine() {
            error!("Failed to terminate sound engine: {:?}", e);
        }
        SCHEDULER.shutdown();
    }
}

pub struct SharedState {
    pub strings: StringContainerShared,
    pub strings_by_activity: HashMap<String, StringContainer>,
    pub config: RwLock<AppConfig>,
    pub renderer_status: RwLock<RendererStatus>,
    pub renderer_capabilities: RwLock<Vec<alkahest_render::renderer::shadowkeep::CapabilityRecord>>,

    /// Investment hash -> Name
    pub activity_names: HashMap<u32, String>,

    /// Activity name hash (eg. mission_sentient) -> Investment hash
    pub activity_hash_to_investment: HashMap<u32, u32>,

    pub wordlist: HashMap<u32, String>,
}

impl SharedState {
    pub fn new() -> anyhow::Result<Self> {
        let mut strings_by_activity: HashMap<String, StringContainer> = HashMap::default();
        for (name, tag) in package_manager().get_named_tags_by_class(0x80808E8B) {
            let Ok(data) = package_manager().read_tag(tag) else {
                continue;
            };
            let mut cur = Cursor::new(data);
            cur.seek(std::io::SeekFrom::Start(0x10))?;
            let hash = WideHash::read_ds(&mut cur)?;
            if hash.is_none() {
                continue;
            }

            let activity = if let Some((_destination, activity)) = name.split_once(".") {
                activity
            } else {
                &name
            };

            let container = StringContainer::load(hash)?;
            match strings_by_activity.entry(activity.to_string()) {
                Entry::Occupied(mut e) => e.get_mut().extend(container),
                Entry::Vacant(e) => {
                    e.insert(container);
                }
            }
        }

        const ACTIVITY_NAME_DATA: &str = include_str!("../assets/data/activity_names.json");
        const ACTIVITY_TO_INVESTENT_DATA: &str =
            include_str!("../assets/data/activity_to_investment.json");

        let mut wordlist = HashMap::default();
        let wordlist_file = std::fs::read_to_string("wordlist.txt").unwrap_or_default();
        for line in wordlist_file.lines() {
            wordlist.insert(fnv1(line), line.to_string());
        }

        let mut s = Self {
            strings: StringContainer::load_all_global().into(),
            strings_by_activity,
            config: RwLock::new(AppConfig::default()),
            renderer_status: RwLock::new(RendererStatus::Initializing),
            renderer_capabilities: RwLock::new(
                alkahest_render::renderer::shadowkeep::bootstrap_capability_ledger(),
            ),
            activity_names: serde_json::from_str(ACTIVITY_NAME_DATA)?,
            activity_hash_to_investment: serde_json::from_str(ACTIVITY_TO_INVESTENT_DATA)?,
            wordlist,
        };

        s.activity_names
            .retain(|_, name| !matches!(name.to_lowercase().as_str(), "solitude"));

        for name in s.activity_names.values_mut() {
            *name = name.replace(": Master", "");
            *name = name.replace(": Standard", "");
            *name = name.replace(": Legendary", "");
            *name = name.replace(": Private", "");
            *name = name.replace(": Matchmade", "");
            *name = name.replace(": Customize", "");
            *name = name.replace(": Contest", "");
            *name = name.replace("Master Conquest: ", "");
            *name = name.replace("Grandmaster Conquest: ", "");
            *name = name.replace("Ultimate Conquest: ", "");
            *name = name.replace("Nightfall Grandmaster: ", "");
        }

        if let Err(e) = s.load_config() {
            warn!("Failed to load config: {:?}", e);
        }

        Ok(s)
    }

    pub fn load_config(&self) -> anyhow::Result<()> {
        let config_path = config_relative_path("config.toml");
        if config_path.exists() {
            let config_str = std::fs::read_to_string(&config_path)?;
            let config: AppConfig = toml::from_str(&config_str)?;
            *self.config.write() = config;
        }

        Ok(())
    }

    pub fn save_config(&self) -> anyhow::Result<()> {
        let config_path = config_relative_path("config.toml");
        let config_str = toml::to_string_pretty(&*self.config.read())?;
        std::fs::write(&config_path, config_str)?;

        Ok(())
    }

    pub fn get_string(&self, hash: u32) -> String {
        self.strings.get(hash)
    }

    pub fn get_string_by_activity(&self, activity_name: &str, hash: u32) -> String {
        self.strings_by_activity
            .get(activity_name)
            .and_then(|s| s.try_get(hash))
            .unwrap_or_else(|| self.get_string(hash))
    }

    pub fn get_activity_name(&self, internal_name: &str) -> Option<&str> {
        let hash = fnv1(internal_name);
        self.activity_hash_to_investment
            .get(&hash)
            .and_then(|investment_hash| {
                self.activity_names.get(investment_hash).map(|s| s.as_str())
            })
    }

    pub fn get_wordlist_string(&self, hash: u32) -> String {
        self.wordlist
            .get(&hash)
            .cloned()
            .unwrap_or_else(|| format!("unk{hash:08X}"))
    }
}
