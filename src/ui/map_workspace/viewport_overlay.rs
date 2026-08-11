use std::{collections::HashMap, hash::BuildHasher};

use alkahest_data::tfx::common::AxisAlignedBBox;
use alkahest_render::camera::Camera;
use egui::{Color32, Pos2, Rect, Response, Ui, vec2};
use glam::{Mat4, Vec3};

use crate::world::shadowkeep_inspection::{MapInspectionNodeId, ShadowkeepMapInspection};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ViewportAction {
    Select(MapInspectionNodeId),
    Focus(MapInspectionNodeId),
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
    _response: &Response,
    inspection: &ShadowkeepMapInspection,
    selected: Option<MapInspectionNodeId>,
    selected_hidden: bool,
    show_selection_bounds: bool,
    show_spawn_markers: bool,
    annotations: &HashMap<u32, String, S>,
) -> Option<ViewportAction> {
    let world_to_clip = camera.projection_matrix_standard() * camera.view_matrix();
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

    if !show_spawn_markers {
        return None;
    }
    let mut action = None;
    for id in &inspection.spawn_nodes {
        let Some(node) = inspection.node(*id) else {
            continue;
        };
        let Some(transform) = node.transform else {
            continue;
        };
        let Some(point) = project_point(world_to_clip, rect, transform.translation) else {
            continue;
        };
        let selected_marker = selected == Some(*id);
        let radius = if selected_marker { 7.0 } else { 4.0 };
        let color = if selected_marker {
            Color32::from_rgb(64, 200, 255)
        } else {
            Color32::from_rgb(255, 210, 80)
        };
        ui.painter().circle_filled(point, radius, color);
        let response = ui.interact(
            Rect::from_center_size(point, vec2(radius * 3.0, radius * 3.0)),
            ui.make_persistent_id(("map_spawn_marker", id.0)),
            egui::Sense::click(),
        );
        let mut tooltip = node
            .name_hash
            .and_then(|hash| annotations.get(&hash))
            .unwrap_or(&node.label)
            .to_owned();
        if let Some(hash) = node.name_hash {
            tooltip.push_str(&format!(" · 0x{hash:08X}"));
        }
        tooltip.push_str(&format!(" · {:?}", transform.translation));
        response.clone().on_hover_text(tooltip);
        if response.double_clicked() {
            action = Some(ViewportAction::Focus(*id));
        } else if response.clicked() {
            action = Some(ViewportAction::Select(*id));
        }
    }
    action
}

#[cfg(test)]
mod tests {
    use super::*;

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
