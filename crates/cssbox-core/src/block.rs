//! Block formatting context layout.
//!
//! Implements CSS 2.1 §9.4.1 — Block Formatting Contexts and
//! CSS 2.1 §10.3.3 — Block-level non-replaced elements in normal flow.

use crate::box_model::{resolve_block_width, BoxModel};
use crate::float::FloatContext;
use crate::fragment::{Fragment, FragmentKind};
use crate::geometry::{Point, Size};
use crate::inline;
use crate::layout::{self, LayoutContext};
use crate::style::{BoxSizing, DisplayInner, Float};
use crate::tree::{FormattingContextType, NodeId};

/// Layout a block-level element and its children.
pub fn layout_block(
    ctx: &LayoutContext,
    node: NodeId,
    containing_block_width: f32,
    containing_block_height: f32,
) -> Fragment {
    let style = ctx.tree.style(node);
    let mut fragment = Fragment::new(node, FragmentKind::Box);
    let forced_height = ctx.take_forced_height(node);

    // 1. Resolve width and margins
    let (content_width, margin) = resolve_block_width(style, containing_block_width);

    // 2. Resolve border and padding
    let border = BoxModel::resolve_border(style);
    let padding = BoxModel::resolve_padding(style, containing_block_width);

    fragment.margin = margin;
    fragment.border = border;
    fragment.padding = padding;

    // 3. Layout children
    let fc_type = ctx.tree.formatting_context(node);

    let mut float_ctx = FloatContext::new(content_width);
    // Floats from an ancestor in the same block formatting context intrude
    // into this block's line boxes.
    for (rect, side) in ctx.take_floats(node) {
        if !style.establishes_bfc() {
            float_ctx.add_exclusion(rect, side);
        }
    }

    let (mut content_height, first_margin_top, last_margin_bottom) = match fc_type {
        FormattingContextType::Inline => (
            layout_inline_children(
                ctx,
                node,
                content_width,
                &mut fragment.children,
                &mut float_ctx,
            ),
            None,
            0.0,
        ),
        _ => layout_block_children(
            ctx,
            node,
            content_width,
            containing_block_height,
            &mut fragment.children,
            &mut float_ctx,
        ),
    };

    // Parent/child margin collapsing (CSS 2.1 §8.3.1): a block without top
    // border/padding that does not establish a BFC shares its first child's
    // top margin; likewise at the bottom for auto-height blocks.
    let independent = style.establishes_bfc()
        || matches!(
            style.display.inner,
            DisplayInner::TableCell | DisplayInner::TableCaption
        )
        || node == ctx.tree.root()
        || ctx.tree.parent(node).is_some_and(|p| {
            matches!(
                ctx.tree.style(p).display.inner,
                DisplayInner::Flex | DisplayInner::Grid
            )
        });
    if !independent && border.top == 0.0 && padding.top == 0.0 {
        if let Some(first) = first_margin_top {
            for child in &mut fragment.children {
                child.position.y -= first;
                if ctx.tree.style(child.node).float != Float::None {
                    child.position.y = child.position.y.max(child.margin.top);
                }
            }
            content_height -= first;
            fragment.margin.top = collapse_margins(fragment.margin.top, first);
        }
    }
    let collapse_bottom = !independent
        && border.bottom == 0.0
        && padding.bottom == 0.0
        && style.height.is_auto()
        && forced_height.is_none();
    if collapse_bottom {
        fragment.margin.bottom = collapse_margins(fragment.margin.bottom, last_margin_bottom);
    } else {
        content_height += last_margin_bottom;
    }

    // 4. Resolve height
    let specified_height = style.height.resolve(containing_block_height);
    let mut final_height = match specified_height {
        Some(mut h) => {
            if style.box_sizing == BoxSizing::BorderBox {
                h = (h - border.vertical() - padding.vertical()).max(0.0);
            }
            h
        }
        None => match forced_height {
            Some(h) => (h - border.vertical() - padding.vertical()).max(0.0),
            None => content_height,
        },
    };

    // Apply min/max height
    let min_h = style.min_height.resolve(containing_block_height);
    let max_h = style
        .max_height
        .resolve(containing_block_height)
        .unwrap_or(f32::INFINITY);
    final_height = final_height.max(min_h).min(max_h);

    // If this block establishes a BFC, it must contain floats
    if style.establishes_bfc() {
        final_height = final_height.max(float_ctx.clear_all());
    }

    fragment.size = Size::new(content_width, final_height);

    fragment
}

/// Layout block-level children within a block formatting context.
///
/// Returns the content height (excluding the last child's bottom margin), the
/// first in-flow child's top margin (if it may collapse with the parent) and
/// the trailing bottom margin.
fn layout_block_children(
    ctx: &LayoutContext,
    parent: NodeId,
    containing_width: f32,
    containing_height: f32,
    fragments: &mut Vec<Fragment>,
    float_ctx: &mut FloatContext,
) -> (f32, Option<f32>, f32) {
    let children = ctx.tree.children(parent);
    let mut cursor_y: f32 = 0.0;
    // Pending (uncollapsed) bottom margin of the previous in-flow child.
    let mut pending_margin: Option<f32> = None;
    let mut first_margin_top: Option<f32> = None;
    let mut seen_in_flow = false;

    for &child_id in children {
        let child_style = ctx.tree.style(child_id);

        // Skip display:none
        if child_style.display.is_none() {
            continue;
        }

        // Handle out-of-flow elements
        if child_style.position.is_absolutely_positioned() {
            // Absolutely positioned elements are laid out later in position::resolve_positioned
            let mut child_fragment =
                layout::layout_node(ctx, child_id, containing_width, containing_height);
            child_fragment.position = Point::ZERO; // placeholder, resolved later
            fragments.push(child_fragment);
            continue;
        }

        // Handle floats
        if child_style.float != Float::None {
            let mut child_fragment =
                layout::layout_node(ctx, child_id, containing_width, containing_height);
            // Floats keep their specified margins (no over-constrained
            // adjustment as for in-flow blocks); auto margins are zero.
            child_fragment.margin = BoxModel::resolve_margin(child_style, containing_width);
            let float_y = cursor_y + pending_margin.unwrap_or(0.0).max(0.0);
            let floated = float_ctx.place_float(child_fragment, child_style.float, float_y);
            fragments.push(floated);
            continue;
        }

        // Hand active floats down so the child's line boxes flow around them.
        if float_ctx.has_floats() && !child_style.establishes_bfc() {
            let bm = BoxModel::resolve(child_style, containing_width);
            let est_offset = match pending_margin {
                Some(prev) => collapse_margins(prev, bm.margin.top),
                None => bm.margin.top,
            };
            let dx = bm.margin.left + bm.border.left + bm.padding.left;
            let dy = cursor_y + est_offset + bm.border.top + bm.padding.top;
            ctx.pass_floats(child_id, float_ctx.exclusions_for_child(dx, dy));
        }

        // Layout the child
        let mut child_fragment =
            layout::layout_node(ctx, child_id, containing_width, containing_height);
        let margin_top = child_fragment.margin.top;
        let margin_bottom = child_fragment.margin.bottom;

        // Margin collapsing between adjacent siblings (CSS 2.1 §8.3.1).
        let mut offset = match pending_margin.take() {
            Some(prev) => collapse_margins(prev, margin_top),
            None => {
                if !seen_in_flow {
                    first_margin_top = Some(margin_top);
                }
                margin_top
            }
        };
        seen_in_flow = true;

        // Handle clear
        if child_style.clear != crate::style::Clear::None {
            let clear_y = float_ctx.clear(child_style.clear);
            if cursor_y + offset < clear_y {
                offset = clear_y - cursor_y;
                first_margin_top = None;
            }
        }
        cursor_y += offset;

        // Position is the top-left of the border box, offset by the left margin.
        child_fragment.position = Point::new(child_fragment.margin.left, cursor_y);

        let height = child_fragment.border_box().height;
        if height == 0.0 {
            // An empty block's own top and bottom margins collapse together
            // and carry on to the next sibling. (It no longer collapses with
            // the parent; a simplification.)
            cursor_y -= offset;
            child_fragment.position.y = cursor_y;
            pending_margin = Some(collapse_margins(offset, margin_bottom));
            first_margin_top = None;
        } else {
            cursor_y += height;
            pending_margin = Some(margin_bottom);
        }

        fragments.push(child_fragment);
    }

    (cursor_y, first_margin_top, pending_margin.unwrap_or(0.0))
}

/// Layout inline-level children within a block container.
fn layout_inline_children(
    ctx: &LayoutContext,
    parent: NodeId,
    containing_width: f32,
    fragments: &mut Vec<Fragment>,
    float_ctx: &mut FloatContext,
) -> f32 {
    inline::layout_inline_formatting_context(ctx, parent, containing_width, fragments, float_ctx)
}

/// Collapse two adjacent margins per CSS 2.1 §8.3.1.
///
/// - Both positive: use the larger.
/// - Both negative: use the more negative (larger absolute value).
/// - One of each: sum them (positive + negative).
fn collapse_margins(margin_a: f32, margin_b: f32) -> f32 {
    if margin_a >= 0.0 && margin_b >= 0.0 {
        margin_a.max(margin_b)
    } else if margin_a < 0.0 && margin_b < 0.0 {
        margin_a.min(margin_b)
    } else {
        margin_a + margin_b
    }
}

/// Compute shrink-to-fit width for a block (CSS 2.1 §10.3.5).
/// Used for floats, inline-blocks, absolutely positioned elements, etc.
pub fn shrink_to_fit_width(ctx: &LayoutContext, node: NodeId, available_width: f32) -> f32 {
    // Preferred width: layout with no constraint (max-content)
    let preferred = compute_intrinsic_width(ctx, node, f32::INFINITY);
    // Preferred minimum width: layout with zero width (min-content)
    let preferred_min = compute_intrinsic_width(ctx, node, 0.0);

    // shrink-to-fit = min(max(preferred minimum, available), preferred)
    preferred_min.max(0.0).max(preferred.min(available_width))
}

/// Compute intrinsic width of a node given a constraint.
fn compute_intrinsic_width(ctx: &LayoutContext, node: NodeId, available: f32) -> f32 {
    let children = ctx.tree.children(node);
    let style = ctx.tree.style(node);

    let _border = BoxModel::resolve_border(style);
    let _padding = BoxModel::resolve_padding(style, available);

    let mut max_child_width: f32 = 0.0;

    for &child_id in children {
        let child_style = ctx.tree.style(child_id);
        if child_style.display.is_none() || child_style.is_out_of_flow() {
            continue;
        }

        if let Some(text) = ctx.tree.node(child_id).text_content() {
            let size = ctx
                .text_measure
                .measure_styled(text, child_style, available);
            max_child_width = max_child_width.max(size.width);
        } else if child_style.display.is_block_level() {
            let child_width = if let Some(w) = child_style.width.resolve(available) {
                w
            } else {
                compute_intrinsic_width(ctx, child_id, available)
            };
            let child_box = BoxModel::resolve(child_style, available);
            max_child_width =
                max_child_width.max(child_width + child_box.horizontal_border_padding());
        }
    }

    max_child_width
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::{compute_layout, FixedWidthTextMeasure};
    use crate::style::ComputedStyle;
    use crate::tree::BoxTreeBuilder;
    use crate::values::{LengthPercentage, LengthPercentageAuto};

    #[test]
    fn test_margin_collapsing_positive() {
        assert_eq!(collapse_margins(10.0, 20.0), 20.0);
    }

    #[test]
    fn test_margin_collapsing_negative() {
        assert_eq!(collapse_margins(-10.0, -20.0), -20.0);
    }

    #[test]
    fn test_margin_collapsing_mixed() {
        assert_eq!(collapse_margins(10.0, -5.0), 5.0);
    }

    #[test]
    fn test_block_with_padding_and_children() {
        let mut builder = BoxTreeBuilder::new();
        let mut root_style = ComputedStyle::block();
        root_style.padding_top = LengthPercentage::px(10.0);
        root_style.padding_bottom = LengthPercentage::px(10.0);
        let root = builder.root(root_style);

        let mut child_style = ComputedStyle::block();
        child_style.height = LengthPercentageAuto::px(100.0);
        builder.element(root, child_style);

        let tree = builder.build();
        let result = compute_layout(&tree, &FixedWidthTextMeasure, Size::new(800.0, 600.0));

        let root_rect = result.bounding_rect(tree.root()).unwrap();
        // Root: 10px top padding + 100px child + 10px bottom padding = 120px border box height
        assert_eq!(root_rect.height, 120.0);
    }

    #[test]
    fn test_nested_blocks() {
        let mut builder = BoxTreeBuilder::new();
        let root = builder.root(ComputedStyle::block());

        let mut outer_style = ComputedStyle::block();
        outer_style.padding_left = LengthPercentage::px(20.0);
        outer_style.padding_right = LengthPercentage::px(20.0);
        let outer = builder.element(root, outer_style);

        let mut inner_style = ComputedStyle::block();
        inner_style.height = LengthPercentageAuto::px(50.0);
        let inner = builder.element(outer, inner_style);

        let tree = builder.build();
        let result = compute_layout(&tree, &FixedWidthTextMeasure, Size::new(800.0, 600.0));

        let inner_rect = result.bounding_rect(inner).unwrap();
        // Inner width: 800 - 20 - 20 = 760
        assert_eq!(inner_rect.width, 760.0);
        // Inner x: outer x (0) + outer border-left (0) + outer padding-left (20)
        assert_eq!(inner_rect.x, 20.0);
    }

    #[test]
    fn test_percentage_width() {
        let mut builder = BoxTreeBuilder::new();
        let root = builder.root(ComputedStyle::block());

        let mut child_style = ComputedStyle::block();
        child_style.width = LengthPercentageAuto::percent(50.0);
        child_style.height = LengthPercentageAuto::px(100.0);
        let child = builder.element(root, child_style);

        let tree = builder.build();
        let result = compute_layout(&tree, &FixedWidthTextMeasure, Size::new(800.0, 600.0));

        let child_rect = result.bounding_rect(child).unwrap();
        assert_eq!(child_rect.width, 400.0); // 50% of 800
    }

    #[test]
    fn test_min_max_width() {
        let mut builder = BoxTreeBuilder::new();
        let root = builder.root(ComputedStyle::block());

        let mut child_style = ComputedStyle::block();
        child_style.width = LengthPercentageAuto::px(1000.0);
        child_style.max_width = crate::values::LengthPercentageNone::px(500.0);
        child_style.height = LengthPercentageAuto::px(50.0);
        let child = builder.element(root, child_style);

        let tree = builder.build();
        let result = compute_layout(&tree, &FixedWidthTextMeasure, Size::new(800.0, 600.0));

        let child_rect = result.bounding_rect(child).unwrap();
        assert_eq!(child_rect.width, 500.0); // clamped by max-width
    }

    #[test]
    fn test_sibling_margins_collapse() {
        let mut builder = BoxTreeBuilder::new();
        let root = builder.root(ComputedStyle::block());
        let mut a = ComputedStyle::block();
        a.height = LengthPercentageAuto::px(10.0);
        a.margin_bottom = LengthPercentageAuto::px(20.0);
        let a = builder.element(root, a);
        let mut b = ComputedStyle::block();
        b.height = LengthPercentageAuto::px(10.0);
        b.margin_top = LengthPercentageAuto::px(15.0);
        b.margin_bottom = LengthPercentageAuto::px(5.0);
        let b = builder.element(root, b);
        let tree = builder.build();
        let result = compute_layout(&tree, &FixedWidthTextMeasure, Size::new(800.0, 600.0));
        assert_eq!(result.bounding_rect(a).unwrap().y, 0.0);
        // max(20, 15) between them.
        assert_eq!(result.bounding_rect(b).unwrap().y, 30.0);
        // The root is a BFC: the last bottom margin stays inside it.
        assert_eq!(result.bounding_rect(root).unwrap().height, 45.0);
    }

    #[test]
    fn test_parent_child_margin_collapse() {
        let mut builder = BoxTreeBuilder::new();
        let root = builder.root(ComputedStyle::block());
        let mut outer = ComputedStyle::block();
        outer.margin_top = LengthPercentageAuto::px(8.0);
        let outer = builder.element(root, outer);
        let mut inner = ComputedStyle::block();
        inner.height = LengthPercentageAuto::px(10.0);
        inner.margin_top = LengthPercentageAuto::px(20.0);
        let inner = builder.element(outer, inner);
        let tree = builder.build();
        let result = compute_layout(&tree, &FixedWidthTextMeasure, Size::new(800.0, 600.0));
        // The 8px and 20px margins collapse to 20px.
        assert_eq!(result.bounding_rect(outer).unwrap().y, 20.0);
        assert_eq!(result.bounding_rect(inner).unwrap().y, 20.0);
        assert_eq!(result.bounding_rect(outer).unwrap().height, 10.0);
    }
}
