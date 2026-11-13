//! Intrinsic (min-content / max-content) widths, CSS 2.1 §10.3.5 and §17.5.2.2.
//!
//! The min-content width is the narrowest a box can be without overflowing
//! (its widest unbreakable piece); the max-content width is its width with
//! no line breaking except forced breaks. Text nodes are unbreakable runs, so
//! embedders that split text into words get word-level minimum widths.

use crate::box_model::BoxModel;
use crate::layout::LayoutContext;
use crate::style::{BoxSizing, FlexDirection};
use crate::tree::{FormattingContextType, NodeId};
use crate::values::LengthPercentageAuto;

/// (min-content, max-content) border-box widths of `node`, excluding margins.
pub fn intrinsic_widths(ctx: &LayoutContext, node: NodeId) -> (f32, f32) {
    let style = ctx.tree.style(node);
    if style.display.is_none() {
        return (0.0, 0.0);
    }
    if let Some(text) = ctx.tree.node(node).text_content() {
        if text == "\n" {
            return (0.0, 0.0);
        }
        let w = ctx
            .text_measure
            .measure_styled(text, style, f32::INFINITY)
            .width;
        return (w, w);
    }
    let bm = BoxModel::resolve(style, 0.0);
    let pb = bm.padding.horizontal() + bm.border.horizontal();
    if let LengthPercentageAuto::Length(w) = style.width {
        let outer = if style.box_sizing == BoxSizing::BorderBox {
            w
        } else {
            w + pb
        };
        return (outer, outer);
    }
    let (min, max) = match ctx.tree.formatting_context(node) {
        FormattingContextType::Table => {
            let (mins, maxs, spacing) = crate::table::column_intrinsics(ctx, node);
            let gaps = spacing * (mins.len() + 1) as f32;
            (
                mins.iter().sum::<f32>() + gaps,
                maxs.iter().sum::<f32>() + gaps,
            )
        }
        FormattingContextType::Inline => inline_widths(ctx, node),
        FormattingContextType::Flex
            if style.flex_direction == FlexDirection::Row
                || style.flex_direction == FlexDirection::RowReverse =>
        {
            let mut min: f32 = 0.0;
            let mut sum = 0.0;
            for (mn, mx) in children_outer(ctx, node) {
                min = min.max(mn);
                sum += mx;
            }
            (min, sum)
        }
        _ => {
            let mut min: f32 = 0.0;
            let mut max: f32 = 0.0;
            for (mn, mx) in children_outer(ctx, node) {
                min = min.max(mn);
                max = max.max(mx);
            }
            (min, max)
        }
    };
    let min = match style.min_width {
        crate::values::LengthPercentage::Length(m) => min.max(m),
        _ => min,
    };
    (min + pb, max.max(min) + pb)
}

/// Outer (margin-box) intrinsic widths of each in-flow child.
fn children_outer(ctx: &LayoutContext, node: NodeId) -> Vec<(f32, f32)> {
    ctx.tree
        .children(node)
        .iter()
        .filter(|&&c| {
            let s = ctx.tree.style(c);
            !s.display.is_none() && !s.position.is_absolutely_positioned()
        })
        .map(|&c| {
            let (mn, mx) = intrinsic_widths(ctx, c);
            let m = margins(ctx, c);
            (mn + m, mx + m)
        })
        .collect()
}

fn margins(ctx: &LayoutContext, node: NodeId) -> f32 {
    let s = ctx.tree.style(node);
    let px = |v: LengthPercentageAuto| match v {
        LengthPercentageAuto::Length(l) => l,
        _ => 0.0,
    };
    px(s.margin_left) + px(s.margin_right)
}

/// Inline formatting context: min = widest item, max = longest line.
fn inline_widths(ctx: &LayoutContext, node: NodeId) -> (f32, f32) {
    let wraps = ctx.tree.style(node).white_space.wraps();
    let mut min: f32 = 0.0;
    let mut max: f32 = 0.0;
    let mut line: f32 = 0.0;
    for &c in ctx.tree.children(node) {
        if ctx.tree.style(c).display.is_none() {
            continue;
        }
        if ctx.tree.node(c).text_content() == Some("\n") {
            max = max.max(line);
            line = 0.0;
            continue;
        }
        let (mn, mx) = intrinsic_widths(ctx, c);
        let m = margins(ctx, c);
        min = min.max(mn + m);
        line += mx + m;
    }
    max = max.max(line);
    if !wraps {
        min = max;
    }
    (min, max)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geometry::Size;
    use crate::layout::FixedWidthTextMeasure;
    use crate::style::ComputedStyle;
    use crate::tree::BoxTreeBuilder;

    #[test]
    fn words_and_breaks() {
        let mut b = BoxTreeBuilder::new();
        let root = b.root(ComputedStyle::block());
        let mut p = ComputedStyle::block();
        p.padding_left = crate::values::LengthPercentage::px(5.0);
        let p = b.element(root, p);
        b.text(p, "aaaa"); // 32px
        b.text(p, "bb"); // 16px
        b.text(p, "\n");
        b.text(p, "ccc"); // 24px
        let tree = b.build();
        let ctx = LayoutContext::new(&tree, &FixedWidthTextMeasure, Size::new(800.0, 600.0));
        // min = widest word (32), max = longest line (48), both plus 5px padding.
        assert_eq!(intrinsic_widths(&ctx, p), (37.0, 53.0));
        assert_eq!(intrinsic_widths(&ctx, root), (37.0, 53.0));
    }
}
