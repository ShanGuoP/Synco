//! Bake the authoritative point-operator chain into a small 3D texture for interaction.
use crate::{error::Result, state::Ctx};
use photoedit_core::{apply_chain, Chain, EditOps};
use serde_json::{json, Value};
use stitch_core::Rgba;
pub fn table(ctx: &Ctx, ops: &EditOps) -> Result<Value> {
    // Neighborhood and geometry operators cannot be represented by a color lookup table.
    if ops.geometry != Default::default()
        || ops.warp != Default::default()
        || !ops.portrait.is_empty()
        || !ops.beauty.is_empty()
        || !ops.color.clarity.is_zero()
        || !ops.color.sharpen.is_zero()
        || ops.develop.detail != Default::default()
        || ops.develop.lens != Default::default()
        || ops
            .develop
            .locals
            .iter()
            .any(|l| l.enabled && (!l.color.is_empty() || !l.look.is_empty()))
        || !ops.develop.effects.vignette.is_zero()
        || !ops.develop.effects.grain.is_zero()
        || !ops.develop.effects.film.is_zero()
    {
        return Ok(json!({"supported":false}));
    }
    const SIZE: usize = 33;
    let mut img = Rgba::new(SIZE, SIZE * SIZE);
    for b in 0..SIZE {
        for g in 0..SIZE {
            for r in 0..SIZE {
                let i = ((b * SIZE + g) * SIZE + r) * 4;
                img.px[i..i + 4].copy_from_slice(&[
                    (r * 255 / (SIZE - 1)) as u8,
                    (g * 255 / (SIZE - 1)) as u8,
                    (b * 255 / (SIZE - 1)) as u8,
                    255,
                ]);
            }
        }
    }
    let lut = ops
        .lut
        .as_ref()
        .map(|l| super::lut_table(ctx, &l.name))
        .transpose()?
        .flatten();
    let out = apply_chain(
        &img,
        ops,
        &Chain {
            lut: lut.as_ref(),
            ..Default::default()
        },
    );
    Ok(json!({"supported":true,"size":SIZE,"rgba":out.px}))
}
