macro_rules! include_icon {
    ($name:ident, $filename:expr) => {
        pub const $name: egui::ImageSource =
            egui::include_image!(concat!("../../assets/ui/icons/", $filename));
    };
}

pub mod director {
    include_icon!(CRUCIBLE, "crucible.svg");
    include_icon!(DUNGEON, "dungeon.svg");
    include_icon!(ENGRAM, "engram.svg");
    include_icon!(GAMBIT, "gambit.svg");
    include_icon!(IRON_BANNER, "iron-banner.svg");
    include_icon!(LOST_SECTOR, "lost-sector.svg");
    include_icon!(OSIRIS, "osiris.svg");
    include_icon!(PATROL, "patrol.svg");
    include_icon!(QUEST, "quest.svg");
    include_icon!(RAID, "raid.svg");
    include_icon!(STRIKE, "strike.svg");
    include_icon!(CINEMATIC, "cinematic.svg");
    include_icon!(UNKNOWN, "unknown.svg");
}

pub mod sequencer {
    include_icon!(AUDIO, "sequencer/audio.png");
    include_icon!(DELAY, "sequencer/delay.png");
    include_icon!(FLOW_PARALLEL, "sequencer/flow_parallel.png");
    include_icon!(FLOW_RANDOM, "sequencer/flow_random.png");
    include_icon!(FLOW_SERIAL, "sequencer/flow_serial.png");
    include_icon!(FLOW_UNKNOWN, "sequencer/flow_unknown.png");
    include_icon!(FX, "sequencer/fx.png");
    include_icon!(LIGHT, "sequencer/light.png");
    include_icon!(PARTICLE_SYSTEM_GPU, "sequencer/particle_system_gpu.png");
    include_icon!(PARTICLE_SYSTEM, "sequencer/particle_system.png");
    include_icon!(UNKNOWN, "sequencer/unknown.png");
}
