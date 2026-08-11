use std::{
    collections::{BTreeSet, HashMap},
    fmt::Write,
    hash::BuildHasher,
};

use alkahest_data::tfx::common::AxisAlignedBBox;
use alkahest_render::camera::{Camera, CameraProjection};
use egui::{Color32, FontId, Pos2, Rect, Response, Stroke, Ui, vec2};
use glam::{Mat4, Vec3};

use crate::{
    ui::scene::SceneDepthSample,
    world::shadowkeep_inspection::{
        MapInspectionDisposition, MapInspectionNodeId, MapInspectionNodeKind,
        ShadowkeepMapInspection,
    },
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ViewportAction {
    Select(MapInspectionNodeId),
    Focus(MapInspectionNodeId),
}

struct ProjectedHelper {
    id: MapInspectionNodeId,
    kind: MapInspectionNodeKind,
    center: Pos2,
    label_rect: Rect,
    label: String,
    depth: f32,
    hidden: bool,
    disposition: MapInspectionDisposition,
    label_visible: bool,
}

impl Default for ProjectedHelper {
    fn default() -> Self {
        Self {
            id: MapInspectionNodeId(0),
            kind: MapInspectionNodeKind::MetadataOnly,
            center: Pos2::ZERO,
            label_rect: Rect::NOTHING,
            label: String::new(),
            depth: 0.0,
            hidden: false,
            disposition: MapInspectionDisposition::NonRendering,
            label_visible: false,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct HoverPickKey {
    world_to_clip: Mat4,
    rect: Rect,
    pointer: Pos2,
    workspace_generation: u64,
}

#[derive(Default)]
pub(super) struct ViewportScratch {
    helpers: Vec<ProjectedHelper>,
    active_helpers: usize,
    occupied_labels: Vec<Rect>,
    hover_pick: Option<(HoverPickKey, Option<MapInspectionNodeId>)>,
}

fn cached_hover_pick(
    scratch: &ViewportScratch,
    key: HoverPickKey,
) -> Option<Option<MapInspectionNodeId>> {
    scratch
        .hover_pick
        .filter(|(cached_key, _)| *cached_key == key)
        .map(|(_, hit)| hit)
}

fn helper_color(
    helper: &ProjectedHelper,
    selected: Option<MapInspectionNodeId>,
    hovered: Option<MapInspectionNodeId>,
) -> Color32 {
    if selected == Some(helper.id) {
        Color32::from_rgb(64, 200, 255)
    } else if hovered == Some(helper.id) {
        Color32::WHITE
    } else if helper.disposition == MapInspectionDisposition::Failed {
        Color32::from_rgb(255, 92, 92)
    } else if helper.disposition == MapInspectionDisposition::Deferred {
        Color32::from_rgb(255, 184, 72)
    } else if helper.hidden {
        Color32::GRAY
    } else {
        Color32::from_rgb(104, 164, 204)
    }
}

fn draw_line(ui: &Ui, points: [Pos2; 2], stroke: Stroke, dashed: bool) {
    if !dashed {
        ui.painter().line_segment(points, stroke);
        return;
    }
    let delta = points[1] - points[0];
    let length = delta.length();
    if length <= f32::EPSILON {
        return;
    }
    let direction = delta / length;
    let mut start = 0.0;
    while start < length {
        let end = (start + 4.0).min(length);
        ui.painter().line_segment(
            [points[0] + direction * start, points[0] + direction * end],
            stroke,
        );
        start += 7.0;
    }
}

fn draw_helper_bounds(
    ui: &Ui,
    world_to_clip: Mat4,
    rect: Rect,
    bounds: AxisAlignedBBox,
    stroke: Stroke,
    dashed: bool,
) {
    let points = bounds_corners(bounds).map(|point| project_point(world_to_clip, rect, point));
    for (left, right) in [
        (0, 1),
        (0, 2),
        (0, 4),
        (1, 3),
        (1, 5),
        (2, 3),
        (2, 6),
        (3, 7),
        (4, 5),
        (4, 6),
        (5, 7),
        (6, 7),
    ] {
        if let (Some(left), Some(right)) = (points[left], points[right]) {
            draw_line(ui, [left, right], stroke, dashed);
        }
    }
}

fn resolve_labels(
    scratch: &mut ViewportScratch,
    enabled: bool,
    selected: Option<MapInspectionNodeId>,
    hovered: Option<MapInspectionNodeId>,
) {
    if !enabled {
        for helper in &mut scratch.helpers[..scratch.active_helpers] {
            helper.label_visible = false;
        }
        return;
    }
    scratch.occupied_labels.clear();
    let helpers = &mut scratch.helpers[..scratch.active_helpers];
    helpers.sort_by(|left, right| {
        let left_special = left.hidden
            || matches!(
                left.disposition,
                MapInspectionDisposition::Failed | MapInspectionDisposition::Deferred
            );
        let right_special = right.hidden
            || matches!(
                right.disposition,
                MapInspectionDisposition::Failed | MapInspectionDisposition::Deferred
            );
        (selected != Some(left.id))
            .cmp(&(selected != Some(right.id)))
            .then_with(|| (hovered != Some(left.id)).cmp(&(hovered != Some(right.id))))
            .then_with(|| right_special.cmp(&left_special))
            .then_with(|| left.depth.total_cmp(&right.depth))
            .then_with(|| left.id.cmp(&right.id))
    });
    for helper in helpers {
        helper.label_visible = !scratch
            .occupied_labels
            .iter()
            .any(|occupied| occupied.intersects(helper.label_rect));
        if helper.label_visible {
            scratch.occupied_labels.push(helper.label_rect);
        }
    }
}

fn helper_hit(scratch: &ViewportScratch, pointer: Pos2) -> Option<MapInspectionNodeId> {
    let helpers = &scratch.helpers[..scratch.active_helpers];
    let label = helpers
        .iter()
        .filter(|helper| helper.label_visible && helper.label_rect.contains(pointer))
        .min_by(|left, right| {
            left.depth
                .total_cmp(&right.depth)
                .then_with(|| left.id.cmp(&right.id))
        })
        .map(|helper| helper.id);
    if label.is_some() {
        return label;
    }
    helpers
        .iter()
        .filter_map(|helper| {
            let distance = helper.center.distance(pointer);
            (distance <= 8.0).then_some((helper.id, distance, helper.depth))
        })
        .min_by(|left, right| {
            left.1
                .total_cmp(&right.1)
                .then_with(|| left.2.total_cmp(&right.2))
                .then_with(|| left.0.cmp(&right.0))
        })
        .map(|(id, _, _)| id)
}

#[derive(Clone, Copy, Debug)]
struct ScreenRay {
    origin: Vec3,
    direction: Vec3,
}

fn unproject_ndc(inverse_world_to_clip: Mat4, ndc: Vec3) -> Option<Vec3> {
    let world = inverse_world_to_clip * ndc.extend(1.0);
    if !world.is_finite() || world.w.abs() <= f32::EPSILON {
        return None;
    }
    let world = world.truncate() / world.w;
    world.is_finite().then_some(world)
}

fn screen_ray(
    camera: &Camera,
    inverse_world_to_clip: Mat4,
    rect: Rect,
    pointer: Pos2,
) -> Option<ScreenRay> {
    if !rect.contains(pointer) || rect.width() <= 0.0 || rect.height() <= 0.0 {
        return None;
    }
    let ndc = Vec3::new(
        ((pointer.x - rect.left()) / rect.width()) * 2.0 - 1.0,
        1.0 - ((pointer.y - rect.top()) / rect.height()) * 2.0,
        0.0,
    );
    let near = unproject_ndc(inverse_world_to_clip, Vec3::new(ndc.x, ndc.y, 1.0))?;
    let far = unproject_ndc(inverse_world_to_clip, Vec3::new(ndc.x, ndc.y, 0.0))?;
    let direction = (far - near).try_normalize()?;
    let origin = match camera.projection {
        CameraProjection::Perspective => camera.position,
        CameraProjection::Orthographic => near,
    };
    Some(ScreenRay { origin, direction })
}

fn ray_aabb(ray: ScreenRay, bounds: AxisAlignedBBox) -> Option<(f32, f32)> {
    if !ray.origin.is_finite()
        || !ray.direction.is_finite()
        || !bounds.min.is_finite()
        || !bounds.max.is_finite()
        || !bounds.min.cmple(bounds.max).all()
    {
        return None;
    }
    let mut enter = f32::NEG_INFINITY;
    let mut exit = f32::INFINITY;
    for axis in 0..3 {
        let origin = ray.origin[axis];
        let direction = ray.direction[axis];
        let min = bounds.min[axis];
        let max = bounds.max[axis];
        if direction.abs() <= 1.0e-7 {
            if origin < min || origin > max {
                return None;
            }
            continue;
        }
        let inverse = direction.recip();
        let mut first = (min - origin) * inverse;
        let mut second = (max - origin) * inverse;
        if first > second {
            std::mem::swap(&mut first, &mut second);
        }
        enter = enter.max(first);
        exit = exit.min(second);
        if enter > exit {
            return None;
        }
    }
    if exit < 0.0 {
        None
    } else {
        Some((enter.max(0.0), exit))
    }
}

fn bounds_volume(bounds: AxisAlignedBBox) -> f32 {
    let size = (bounds.max - bounds.min).truncate().max(Vec3::ZERO);
    size.x * size.y * size.z
}

fn bounds_distance(bounds: AxisAlignedBBox, point: Vec3) -> f32 {
    let min = bounds.min.truncate();
    let max = bounds.max.truncate();
    let delta = (min - point).max(Vec3::ZERO) + (point - max).max(Vec3::ZERO);
    delta.length()
}

fn nearest_surface_hit(
    ray: ScreenRay,
    inspection: &ShadowkeepMapInspection,
    locator_rows: &[MapInspectionNodeId],
    visible_rows: &BTreeSet<MapInspectionNodeId>,
) -> Option<MapInspectionNodeId> {
    let mut best: Option<(MapInspectionNodeId, f32, f32)> = None;
    for &id in locator_rows {
        if !visible_rows.contains(&id) {
            continue;
        }
        let Some(bounds) = inspection.node(id).and_then(|node| node.bounds) else {
            continue;
        };
        let Some((entry, _)) = ray_aabb(ray, bounds) else {
            continue;
        };
        let volume = bounds_volume(bounds);
        let replace = best.is_none_or(|(best_id, best_entry, best_volume)| {
            entry < best_entry - 1.0e-4
                || ((entry - best_entry).abs() <= 1.0e-4
                    && (volume < best_volume || (volume == best_volume && id < best_id)))
        });
        if replace {
            best = Some((id, entry, volume));
        }
    }
    best.map(|(id, _, _)| id)
}

fn depth_surface_hit(
    ray: ScreenRay,
    sample: SceneDepthSample,
    inspection: &ShadowkeepMapInspection,
    locator_rows: &[MapInspectionNodeId],
    visible_rows: &BTreeSet<MapInspectionNodeId>,
) -> Option<MapInspectionNodeId> {
    let sampled_distance = sample.world_position.distance(ray.origin);
    let mut candidates = Vec::new();
    for &id in locator_rows {
        if !visible_rows.contains(&id) {
            continue;
        }
        let Some(bounds) = inspection.node(id).and_then(|node| node.bounds) else {
            continue;
        };
        let Some((entry, _)) = ray_aabb(ray, bounds) else {
            continue;
        };
        let distance = bounds_distance(bounds, sample.world_position);
        let contained = distance <= f32::EPSILON;
        candidates.push((
            id,
            bounds,
            distance,
            contained,
            bounds_volume(bounds),
            (entry - sampled_distance).abs(),
        ));
    }
    candidates.sort_by(|left, right| {
        left.2
            .total_cmp(&right.2)
            .then_with(|| right.3.cmp(&left.3))
            .then_with(|| left.4.total_cmp(&right.4))
            .then_with(|| left.5.total_cmp(&right.5))
            .then_with(|| left.0.cmp(&right.0))
    });
    candidates
        .first()
        .and_then(|&(id, bounds, distance, contained, _, _)| {
            let radius = (bounds.max - bounds.min).truncate().length() * 0.5;
            let tolerance = (radius * 0.1).min(2.0).max(0.25);
            (contained || distance <= tolerance).then_some(id)
        })
}

pub fn project_point(world_to_clip: Mat4, rect: Rect, point: Vec3) -> Option<Pos2> {
    let clip = world_to_clip * point.extend(1.0);
    if !clip.is_finite() || clip.w <= 0.0 {
        return None;
    }
    let ndc = clip.truncate() / clip.w;
    if !ndc.is_finite() || ndc.x.abs() > 1.0 || ndc.y.abs() > 1.0 {
        return None;
    }
    Some(
        rect.left_top()
            + vec2(
                (ndc.x + 1.0) * 0.5 * rect.width(),
                (1.0 - ndc.y) * 0.5 * rect.height(),
            ),
    )
}

fn bounds_corners(bounds: AxisAlignedBBox) -> [Vec3; 8] {
    std::array::from_fn(|index| {
        Vec3::new(
            if index & 1 == 0 {
                bounds.min.x
            } else {
                bounds.max.x
            },
            if index & 2 == 0 {
                bounds.min.y
            } else {
                bounds.max.y
            },
            if index & 4 == 0 {
                bounds.min.z
            } else {
                bounds.max.z
            },
        )
    })
}

fn draw_selected_bounds(
    ui: &Ui,
    world_to_clip: Mat4,
    rect: Rect,
    bounds: AxisAlignedBBox,
    hidden: bool,
) {
    let points = bounds_corners(bounds).map(|point| project_point(world_to_clip, rect, point));
    let color = if hidden {
        Color32::GRAY
    } else {
        Color32::from_rgb(64, 200, 255)
    };
    for (left, right) in [
        (0, 1),
        (0, 2),
        (0, 4),
        (1, 3),
        (1, 5),
        (2, 3),
        (2, 6),
        (3, 7),
        (4, 5),
        (4, 6),
        (5, 7),
        (6, 7),
    ] {
        if let (Some(left), Some(right)) = (points[left], points[right]) {
            ui.painter().line_segment([left, right], (2.0, color));
        }
    }
}

pub fn show<S: BuildHasher>(
    ui: &mut Ui,
    rect: Rect,
    camera: &Camera,
    response: &Response,
    depth_sample: Option<SceneDepthSample>,
    inspection: &ShadowkeepMapInspection,
    locator_rows: &[MapInspectionNodeId],
    visible_locator_rows: &BTreeSet<MapInspectionNodeId>,
    hovered: &mut Option<MapInspectionNodeId>,
    selected: Option<MapInspectionNodeId>,
    selected_hidden: bool,
    show_selection_bounds: bool,
    scratch: &mut ViewportScratch,
    show_visual_helpers: bool,
    show_visual_labels: bool,
    show_spawn_markers: bool,
    workspace_generation: u64,
    annotations: &HashMap<u32, String, S>,
) -> Option<ViewportAction> {
    let world_to_clip = camera.projection_matrix_standard() * camera.view_matrix();
    let inverse_world_to_clip = camera.world_to_clip_space().inverse();
    let pointer = ui
        .ctx()
        .pointer_hover_pos()
        .filter(|pointer| rect.contains(*pointer));
    *hovered = if let Some(pointer) = pointer {
        let key = HoverPickKey {
            world_to_clip,
            rect,
            pointer,
            workspace_generation,
        };
        if let Some(cached_hit) = cached_hover_pick(scratch, key) {
            cached_hit
        } else {
            let hit = screen_ray(camera, inverse_world_to_clip, rect, pointer).and_then(|ray| {
                nearest_surface_hit(ray, inspection, locator_rows, visible_locator_rows)
            });
            scratch.hover_pick = Some((key, hit));
            hit
        }
    } else {
        scratch.hover_pick = None;
        None
    };
    if show_selection_bounds {
        if let Some(node) = selected.and_then(|id| inspection.node(id)) {
            let color = if selected_hidden {
                Color32::GRAY
            } else {
                Color32::from_rgb(64, 200, 255)
            };
            if let Some(bounds) = node.bounds {
                draw_selected_bounds(ui, world_to_clip, rect, bounds, selected_hidden);
            } else if let Some(transform) = node.transform
                && let Some(point) = project_point(world_to_clip, rect, transform.translation)
            {
                ui.painter().circle_stroke(point, 8.0, (2.0, color));
                ui.painter().line_segment(
                    [point - vec2(12.0, 0.0), point + vec2(12.0, 0.0)],
                    (2.0, color),
                );
                ui.painter().line_segment(
                    [point - vec2(0.0, 12.0), point + vec2(0.0, 12.0)],
                    (2.0, color),
                );
                ui.painter().text(
                    point + vec2(12.0, -12.0),
                    egui::Align2::LEFT_BOTTOM,
                    &node.label,
                    egui::FontId::proportional(12.0),
                    color,
                );
            }
        }
    }
    let mut action = None;
    scratch.active_helpers = 0;
    if show_visual_helpers || show_spawn_markers {
        for &id in locator_rows {
            let Some(node) = inspection.node(id) else {
                continue;
            };
            if node.kind == MapInspectionNodeKind::SpawnPoint && !show_spawn_markers {
                continue;
            }
            if !show_visual_helpers && node.kind != MapInspectionNodeKind::SpawnPoint {
                continue;
            }
            let anchor = node
                .bounds
                .map(|bounds| (bounds.min + bounds.max).truncate() * 0.5)
                .or_else(|| node.transform.map(|transform| transform.translation));
            let Some(anchor) = anchor.filter(|anchor| anchor.is_finite()) else {
                continue;
            };
            let Some(center) = project_point(world_to_clip, rect, anchor) else {
                continue;
            };
            let index = scratch.active_helpers;
            if index == scratch.helpers.len() {
                scratch.helpers.push(ProjectedHelper::default());
            }
            let helper = &mut scratch.helpers[index];
            helper.id = id;
            helper.kind = node.kind;
            helper.center = center;
            helper.depth = anchor.distance(camera.position);
            helper.hidden = !visible_locator_rows.contains(&id);
            helper.disposition = node.disposition;
            helper.label_visible = false;
            if show_visual_helpers && show_visual_labels {
                helper.label.clear();
                let _ = write!(helper.label, "#{} {:?} {}", id.0, node.kind, node.label);
                if let Some(tag) = node.tag {
                    let _ = write!(helper.label, " [{tag}]");
                }
                let label_width =
                    (helper.label.chars().count() as f32 * 6.5 + 8.0).clamp(48.0, 280.0);
                helper.label_rect =
                    Rect::from_min_size(center + vec2(8.0, -8.0), vec2(label_width, 16.0));
            }
            scratch.active_helpers += 1;
        }
    }

    let labels_enabled = show_visual_helpers && show_visual_labels;
    if labels_enabled {
        resolve_labels(scratch, true, selected, *hovered);
    }
    let helper_hovered = pointer.and_then(|pointer| helper_hit(scratch, pointer));
    if helper_hovered.is_some() {
        *hovered = helper_hovered;
        if labels_enabled {
            resolve_labels(scratch, true, selected, *hovered);
        }
    }

    for helper in &scratch.helpers[..scratch.active_helpers] {
        let Some(node) = inspection.node(helper.id) else {
            continue;
        };
        let color = helper_color(helper, selected, *hovered);
        if show_visual_helpers {
            if let Some(bounds) = node.bounds {
                draw_helper_bounds(
                    ui,
                    world_to_clip,
                    rect,
                    bounds,
                    Stroke::new(1.0, color),
                    helper.hidden,
                );
            } else {
                draw_line(
                    ui,
                    [
                        helper.center - vec2(6.0, 0.0),
                        helper.center + vec2(6.0, 0.0),
                    ],
                    Stroke::new(1.0, color),
                    helper.hidden,
                );
                draw_line(
                    ui,
                    [
                        helper.center - vec2(0.0, 6.0),
                        helper.center + vec2(0.0, 6.0),
                    ],
                    Stroke::new(1.0, color),
                    helper.hidden,
                );
            }
        }
        if show_spawn_markers && node.kind == MapInspectionNodeKind::SpawnPoint {
            ui.painter().circle_filled(
                helper.center,
                if selected == Some(helper.id) {
                    7.0
                } else {
                    5.0
                },
                Color32::from_rgb(255, 210, 80),
            );
        }
        if show_visual_helpers {
            let chip = Rect::from_center_size(helper.center, vec2(16.0, 16.0));
            ui.painter()
                .rect_filled(chip, 2.0, Color32::from_black_alpha(210));
            ui.painter().text(
                chip.center(),
                egui::Align2::CENTER_CENTER,
                super::map_node_icon(helper.kind).to_string(),
                FontId::proportional(14.0),
                color,
            );
            if helper.label_visible {
                ui.painter()
                    .rect_filled(helper.label_rect, 2.0, Color32::from_black_alpha(196));
                ui.painter().text(
                    helper.label_rect.left_center() + vec2(4.0, 0.0),
                    egui::Align2::LEFT_CENTER,
                    &helper.label,
                    FontId::monospace(10.5),
                    color,
                );
            }
        }
    }

    if let Some(id) = helper_hovered
        && let Some(node) = inspection.node(id)
    {
        let position = node
            .bounds
            .map(|bounds| (bounds.min + bounds.max).truncate() * 0.5)
            .or_else(|| node.transform.map(|transform| transform.translation));
        let extents = node
            .bounds
            .map(|bounds| (bounds.max - bounds.min).truncate() * 0.5);
        let owner_visible = visible_locator_rows.contains(&id);
        let annotation = node
            .name_hash
            .and_then(|hash| annotations.get(&hash))
            .map(String::as_str)
            .unwrap_or("");
        response.clone().on_hover_text(format!(
            "{}\nTag/hash: {}{}\nStatus: {}\nOwner: {}\nPosition: {:?}\nExtents: {:?}",
            inspection.breadcrumb(id),
            node.tag
                .map(|tag| tag.to_string())
                .or_else(|| node.class.map(|class| format!("0x{class:08X}")))
                .unwrap_or_else(|| "none".to_owned()),
            if annotation.is_empty() {
                String::new()
            } else {
                format!(" · {annotation}")
            },
            node.disposition.status_label(),
            if owner_visible {
                "visible"
            } else {
                "hidden/unbound"
            },
            position,
            extents,
        ));
    }

    if helper_hovered.is_some()
        && (response.clicked_by(egui::PointerButton::Primary)
            || response.double_clicked_by(egui::PointerButton::Primary))
        && let Some(id) = helper_hovered
    {
        action = Some(
            if response.double_clicked_by(egui::PointerButton::Primary) {
                ViewportAction::Focus(id)
            } else {
                ViewportAction::Select(id)
            },
        );
    }
    if action.is_none()
        && (response.clicked_by(egui::PointerButton::Primary)
            || response.double_clicked_by(egui::PointerButton::Primary))
        && let Some(pointer) = response.interact_pointer_pos()
        && let Some(ray) = screen_ray(camera, inverse_world_to_clip, rect, pointer)
    {
        let picked = depth_sample
            .and_then(|sample| {
                depth_surface_hit(ray, sample, inspection, locator_rows, visible_locator_rows)
            })
            .or_else(|| nearest_surface_hit(ray, inspection, locator_rows, visible_locator_rows));
        if let Some(id) = picked {
            action = Some(
                if response.double_clicked_by(egui::PointerButton::Primary) {
                    ViewportAction::Focus(id)
                } else {
                    ViewportAction::Select(id)
                },
            );
        }
    }
    action
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bounds(center: Vec3, extents: Vec3) -> AxisAlignedBBox {
        AxisAlignedBBox::from_center_extents(center, extents)
    }

    #[test]
    fn viewport_pick_rays_cover_perspective_orthographic_and_edges() {
        let rect = Rect::from_min_size(Pos2::ZERO, vec2(200.0, 100.0));
        let mut perspective = Camera::default();
        perspective.aspect_ratio = 2.0;
        let perspective_inverse = perspective.world_to_clip_space().inverse();
        let center = screen_ray(&perspective, perspective_inverse, rect, rect.center()).unwrap();
        assert!(center.direction.dot(Vec3::X) > 0.999);
        let edge = screen_ray(
            &perspective,
            perspective_inverse,
            rect,
            Pos2::new(199.0, 50.0),
        )
        .unwrap();
        assert!(edge.direction.y < 0.0);

        let mut orthographic = Camera::default();
        orthographic.projection = CameraProjection::Orthographic;
        orthographic.aspect_ratio = 2.0;
        let orthographic_inverse = orthographic.world_to_clip_space().inverse();
        let ortho_center =
            screen_ray(&orthographic, orthographic_inverse, rect, rect.center()).unwrap();
        let ortho_edge = screen_ray(
            &orthographic,
            orthographic_inverse,
            rect,
            Pos2::new(199.0, 50.0),
        )
        .unwrap();
        assert!(ortho_center.direction.dot(ortho_edge.direction) > 0.9999);
        assert!(ortho_center.origin.distance(ortho_edge.origin) > 0.1);
    }

    #[test]
    fn viewport_pick_ray_aabb_handles_parallel_inside_behind_and_invalid() {
        let ray = ScreenRay {
            origin: Vec3::ZERO,
            direction: Vec3::X,
        };
        assert_eq!(
            ray_aabb(ray, bounds(Vec3::new(5.0, 0.0, 0.0), Vec3::ONE)),
            Some((4.5, 5.5))
        );
        assert_eq!(
            ray_aabb(
                ScreenRay {
                    origin: Vec3::new(5.0, 0.0, 0.0),
                    direction: Vec3::X,
                },
                bounds(Vec3::new(5.0, 0.0, 0.0), Vec3::ONE),
            ),
            Some((0.0, 0.5))
        );
        assert!(ray_aabb(ray, bounds(Vec3::new(5.0, 3.0, 0.0), Vec3::ONE)).is_none());
        assert!(ray_aabb(ray, bounds(Vec3::new(-5.0, 0.0, 0.0), Vec3::ONE)).is_none());
        assert!(
            ray_aabb(
                ScreenRay {
                    origin: Vec3::NAN,
                    direction: Vec3::X,
                },
                bounds(Vec3::ZERO, Vec3::ONE),
            )
            .is_none()
        );
    }

    #[test]
    fn viewport_pick_depth_prefers_specific_visible_instance() {
        use tiger_pkg::TagHash;

        use crate::world::shadowkeep_inspection::{
            MapInspectionDisposition, MapInspectionGraphBuilder, MapInspectionNode,
            MapInspectionNodeKind, ShadowkeepTableSources,
        };

        let mut builder = MapInspectionGraphBuilder::new(TagHash(1), TagHash(2));
        let root = builder.root();
        let mut owner = MapInspectionNode::new(
            MapInspectionNodeKind::StaticGeometry,
            MapInspectionDisposition::Rendering,
            "Owner",
            ShadowkeepTableSources::default(),
        );
        owner.bounds = Some(bounds(Vec3::new(6.0, 0.0, 0.0), Vec3::splat(6.0)));
        let owner = builder.add_node(Some(root), owner);
        let mut instance = MapInspectionNode::new(
            MapInspectionNodeKind::StaticInstance,
            MapInspectionDisposition::Rendering,
            "Instance",
            ShadowkeepTableSources::default(),
        );
        instance.visual_owner = Some(owner);
        instance.bounds = Some(bounds(Vec3::new(5.0, 0.0, 0.0), Vec3::ONE));
        let instance = builder.add_node(Some(owner), instance);
        let inspection = builder.finalize();
        let ray = ScreenRay {
            origin: Vec3::ZERO,
            direction: Vec3::X,
        };
        let sample = SceneDepthSample {
            pixel: glam::UVec2::ZERO,
            reverse_depth: 1.0,
            world_position: Vec3::new(5.0, 0.0, 0.0),
        };
        let visible = BTreeSet::from([owner, instance]);
        assert_eq!(
            depth_surface_hit(
                ray,
                sample,
                &inspection,
                inspection.locator_nodes(),
                &visible
            ),
            Some(instance)
        );
        assert_eq!(
            depth_surface_hit(
                ray,
                sample,
                &inspection,
                inspection.locator_nodes(),
                &BTreeSet::from([owner]),
            ),
            Some(owner)
        );
    }

    #[test]
    fn viewport_helper_collision_and_manual_hit_are_deterministic() {
        let mut scratch = ViewportScratch::default();
        for id in [MapInspectionNodeId(4), MapInspectionNodeId(2)] {
            let helper = ProjectedHelper {
                id,
                center: Pos2::new(20.0 + id.0 as f32, 20.0),
                label_rect: Rect::from_min_size(Pos2::new(30.0, 10.0), vec2(80.0, 16.0)),
                depth: 5.0,
                hidden: id.0 == 4,
                ..Default::default()
            };
            scratch.helpers.push(helper);
            scratch.active_helpers += 1;
        }
        resolve_labels(&mut scratch, true, Some(MapInspectionNodeId(2)), None);
        assert_eq!(
            scratch.helpers[..scratch.active_helpers]
                .iter()
                .filter(|helper| helper.label_visible)
                .map(|helper| helper.id)
                .collect::<Vec<_>>(),
            vec![MapInspectionNodeId(2)]
        );
        assert_eq!(
            helper_hit(&scratch, Pos2::new(40.0, 15.0)),
            Some(MapInspectionNodeId(2))
        );
        assert_eq!(
            helper_hit(&scratch, Pos2::new(24.0, 20.0)),
            Some(MapInspectionNodeId(4))
        );
    }

    #[test]
    fn hover_pick_cache_requires_the_complete_key() {
        let key = HoverPickKey {
            world_to_clip: Mat4::IDENTITY,
            rect: Rect::from_min_size(Pos2::ZERO, vec2(100.0, 50.0)),
            pointer: Pos2::new(10.0, 20.0),
            workspace_generation: 7,
        };
        let hit = Some(MapInspectionNodeId(4));
        let mut scratch = ViewportScratch::default();
        scratch.hover_pick = Some((key, hit));
        assert_eq!(cached_hover_pick(&scratch, key), Some(hit));
        for changed in [
            HoverPickKey {
                world_to_clip: Mat4::from_translation(Vec3::X),
                ..key
            },
            HoverPickKey {
                rect: Rect::from_min_size(Pos2::ZERO, vec2(101.0, 50.0)),
                ..key
            },
            HoverPickKey {
                pointer: Pos2::new(11.0, 20.0),
                ..key
            },
            HoverPickKey {
                workspace_generation: 8,
                ..key
            },
        ] {
            assert_eq!(cached_hover_pick(&scratch, changed), None);
        }
    }

    #[test]
    fn disabled_label_resolution_preserves_order_and_text() {
        let mut scratch = ViewportScratch::default();
        for id in [MapInspectionNodeId(4), MapInspectionNodeId(2)] {
            scratch.helpers.push(ProjectedHelper {
                id,
                label: format!("label {}", id.0),
                label_visible: true,
                ..Default::default()
            });
            scratch.active_helpers += 1;
        }
        resolve_labels(&mut scratch, false, Some(MapInspectionNodeId(2)), None);
        assert_eq!(
            scratch.helpers[..scratch.active_helpers]
                .iter()
                .map(|helper| (helper.id, helper.label.as_str(), helper.label_visible))
                .collect::<Vec<_>>(),
            vec![
                (MapInspectionNodeId(4), "label 4", false),
                (MapInspectionNodeId(2), "label 2", false),
            ]
        );
        assert!(scratch.occupied_labels.is_empty());
    }
    #[test]
    fn projection_rejects_behind_points_and_is_stable() {
        let rect = Rect::from_min_size(Pos2::ZERO, vec2(200.0, 100.0));
        let matrix = Mat4::from_cols(
            glam::Vec4::X,
            glam::Vec4::Y,
            glam::Vec4::new(0.0, 0.0, 1.0, 1.0),
            glam::Vec4::ZERO,
        );
        assert!(project_point(matrix, rect, Vec3::new(0.0, 0.0, -1.0)).is_none());
        let projected = project_point(matrix, rect, Vec3::new(0.0, 0.0, 1.0)).unwrap();
        assert_eq!(projected, Pos2::new(100.0, 50.0));
    }
}
