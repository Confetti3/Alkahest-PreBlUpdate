use ahash::HashSet;
use alkahest_render::{Renderer, tfx::sequencer_vm::global_channel::GlobalChannelExpression};

pub fn s_evaluate_global_channel_expressions(world: &hecs::World) {
    let renderer = Renderer::instance();
    let mut externs = renderer.externs.write();
    for (_entity, gce) in world.query::<&mut GlobalChannelExpression>().iter() {
        gce.evaluate(&mut externs);
    }
}

pub fn s_get_all_global_channel_ids(world: &hecs::World) -> HashSet<u32> {
    let mut ids = HashSet::default();
    for (_entity, gce) in world.query::<&GlobalChannelExpression>().iter() {
        ids.insert(gce.channel_id);
    }

    ids
}
