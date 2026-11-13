//! Inline formatting context implementation.
//!
//! This module handles the layout of inline-level content, including text runs,
//! inline boxes, and line breaking.

use crate::box_model::BoxModel;
use crate::float::FloatContext;
use crate::fragment::{Fragment, FragmentKind};
use crate::geometry::{Edges, Point, Size};
use crate::layout::LayoutContext;
use crate::style::{BoxSizing, TextAlign, VerticalAlign, WhiteSpace};
use crate::tree::NodeId;

/// An inline item that needs to be laid out within a line.
#[derive(Debug, Clone)]
struct InlineItem {
    node: NodeId,
    /// Outer size used for line breaking (margin box for atomic inlines).
    size: Size,
    baseline: f32,
    /// Atomic inline-level box (replaced element or explicitly sized box).
    atomic: Option<AtomicBox>,
    /// A forced line break (a text node consisting of exactly `"\n"`).
    forced_break: bool,
}

/// Resolved box model of an atomic inline box.
#[derive(Debug, Clone, Copy)]
struct AtomicBox {
    content: Size,
    margin: Edges,
    border: Edges,
    padding: Edges,
}

/// A single line box containing inline items.
#[derive(Debug)]
struct LineBox {
    items: Vec<InlineItem>,
    width: f32,
    height: f32,
    baseline: f32,
    descent: f32,
}

impl LineBox {
    fn new() -> Self {
        Self {
            items: Vec::new(),
            width: 0.0,
            height: 0.0,
            baseline: 0.0,
            descent: 0.0,
        }
    }

    fn add_item(&mut self, item: InlineItem) {
        self.width += item.size.width;
        self.baseline = self.baseline.max(item.baseline);
        self.descent = self.descent.max(item.size.height - item.baseline);
        // Items are baseline-aligned, so the line must fit the tallest ascent
        // plus the deepest descent, not merely the tallest item.
        self.height = self
            .height
            .max(item.size.height)
            .max(self.baseline + self.descent);
        self.items.push(item);
    }

    fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    fn used_width(&self) -> f32 {
        self.width
    }
}

/// Layout an inline formatting context.
///
/// This function creates line boxes from inline-level children of the parent node,
/// applying line breaking, text alignment, and vertical alignment.
///
/// Text nodes are treated as unbreakable runs; embedders wanting word-level
/// wrapping should split text into one node per word. A text node whose
/// content is exactly `"\n"` forces a line break. Inline elements with a
/// resolvable `width` and `height` are laid out as atomic inline boxes.
///
/// # Returns
///
/// The total height consumed by all line boxes.
pub fn layout_inline_formatting_context(
    ctx: &LayoutContext,
    parent: NodeId,
    containing_width: f32,
    fragments: &mut Vec<Fragment>,
    float_ctx: &mut FloatContext,
) -> f32 {
    let parent_style = ctx.tree.style(parent);
    let text_align = parent_style.text_align;
    let line_height = parent_style.line_height;
    let white_space = parent_style.white_space;

    // Collect inline items from children
    let mut inline_items = Vec::new();
    collect_inline_items(ctx, parent, containing_width, &mut inline_items);

    // Break into line boxes, narrowing each line by any floats beside it.
    let line_boxes = flow_lines(
        &inline_items,
        containing_width,
        line_height,
        white_space,
        float_ctx,
    );

    // Position line boxes and create fragments
    let line_count = line_boxes.len();

    for (line_idx, (line_box, line_y, line_left, line_width)) in line_boxes.iter().enumerate() {
        let current_y = *line_y;
        let containing_width = *line_width;
        let line_height_actual = line_box.height.max(line_height);
        // Extra leading is split evenly above and below the content.
        let half_leading = ((line_height_actual - line_box.height) / 2.0).max(0.0);
        let ends_paragraph =
            line_idx + 1 == line_count || line_box.items.last().is_some_and(|i| i.forced_break);
        let justify = text_align == TextAlign::Justify && !ends_paragraph;

        // Apply text alignment
        let line_offset_x = calculate_text_align_offset(
            text_align,
            containing_width,
            line_box.used_width(),
            line_box.items.len(),
        );

        // Create line box fragment
        let mut line_fragment = Fragment::new(parent, FragmentKind::LineBox);
        line_fragment.position = Point::new(*line_left, current_y);
        line_fragment.size = Size::new(containing_width, line_height_actual);

        // Position items within line
        let mut current_x = line_offset_x;
        let gap_count = line_box.items.iter().filter(|i| !i.forced_break).count();

        for (idx, item) in line_box.items.iter().enumerate() {
            if item.forced_break {
                continue;
            }
            let item_style = ctx.tree.style(item.node);
            let vertical_offset = half_leading
                + calculate_vertical_align_offset(
                    item_style.vertical_align,
                    line_box.baseline,
                    item.baseline,
                    item.size.height,
                    line_box.height,
                );

            let item_fragment = match item.atomic {
                Some(atomic) => {
                    let mut f = Fragment::new(item.node, FragmentKind::Box);
                    f.size = atomic.content;
                    f.margin = atomic.margin;
                    f.border = atomic.border;
                    f.padding = atomic.padding;
                    f.position = Point::new(
                        current_x + atomic.margin.left,
                        vertical_offset + atomic.margin.top,
                    );
                    f
                }
                None => {
                    let mut f = Fragment::new(item.node, FragmentKind::TextRun);
                    f.size = item.size;
                    f.position = Point::new(current_x, vertical_offset);
                    f
                }
            };

            // Apply justify spacing if needed
            let extra_space = if justify && gap_count > 1 && idx + 1 < gap_count {
                let remaining = (containing_width - line_box.used_width()).max(0.0);
                remaining / (gap_count - 1) as f32
            } else {
                0.0
            };

            current_x += item.size.width + extra_space;

            line_fragment.children.push(item_fragment);
        }

        fragments.push(line_fragment);
    }

    line_boxes
        .last()
        .map(|(lb, y, _, _)| y + lb.height.max(line_height))
        .unwrap_or(0.0)
}

/// Greedy line breaking where each line's available width can be reduced by
/// floats. Returns (line, y, left offset, available width) per line.
fn flow_lines(
    items: &[InlineItem],
    containing_width: f32,
    line_height: f32,
    white_space: WhiteSpace,
    float_ctx: &FloatContext,
) -> Vec<(LineBox, f32, f32, f32)> {
    let mut out = Vec::new();
    if items.is_empty() {
        return out;
    }
    let allow_wrapping = white_space.wraps();
    let floats = float_ctx.has_floats();
    let mut y = 0.0f32;
    let mut idx = 0;
    while idx < items.len() {
        let probe = line_height.max(1.0);
        let (mut left, mut avail) = if floats {
            float_ctx.available_width_at(y, probe)
        } else {
            (0.0, containing_width)
        };
        // If the next item cannot fit beside the floats, move below them.
        if floats && allow_wrapping {
            let first_w = items[idx].size.width;
            let mut guard = 0;
            while first_w > avail && avail < containing_width - 0.5 && guard < 64 {
                let ny = float_ctx.next_y_after(y, probe);
                if ny <= y {
                    break;
                }
                y = ny;
                let a = float_ctx.available_width_at(y, probe);
                left = a.0;
                avail = a.1;
                guard += 1;
            }
        }
        let mut line = LineBox::new();
        while idx < items.len() {
            let item = &items[idx];
            if item.forced_break {
                line.add_item(item.clone());
                idx += 1;
                break;
            }
            if line.is_empty() || !allow_wrapping || line.used_width() + item.size.width <= avail {
                line.add_item(item.clone());
                idx += 1;
            } else {
                break;
            }
        }
        let h = line.height.max(line_height);
        out.push((line, y, left, avail));
        y += h;
    }
    out
}

/// Collect inline items from the children of a parent node.
fn collect_inline_items(
    ctx: &LayoutContext,
    parent: NodeId,
    max_width: f32,
    items: &mut Vec<InlineItem>,
) {
    let children = ctx.tree.children(parent);

    for &child in children {
        let node = ctx.tree.node(child);
        let style = ctx.tree.style(child);

        if style.display.is_none() {
            continue;
        }

        if node.is_text() {
            if let Some(text) = node.text_content() {
                if text == "\n" {
                    let size = ctx.text_measure.measure_styled("", style, max_width);
                    items.push(InlineItem {
                        node: child,
                        size: Size::new(0.0, size.height),
                        baseline: size.height * 0.8,
                        atomic: None,
                        forced_break: true,
                    });
                    continue;
                }

                let size = ctx.text_measure.measure_styled(text, style, max_width);

                // For text, baseline is typically at bottom minus descent
                // Simplified: baseline is at 80% of height (20% descent)
                let baseline = size.height * 0.8;

                items.push(InlineItem {
                    node: child,
                    size,
                    baseline,
                    atomic: None,
                    forced_break: false,
                });
            }
        } else if let (Some(w), Some(h)) = (
            style.width.resolve(max_width),
            style.height.resolve(ctx.viewport.height),
        ) {
            // Atomic inline box with an explicit size (images, form controls,
            // explicitly sized inline-blocks).
            let border = BoxModel::resolve_border(style);
            let padding = BoxModel::resolve_padding(style, max_width);
            let margin = BoxModel::resolve_margin(style, max_width);
            let (cw, ch) = if style.box_sizing == BoxSizing::BorderBox {
                (
                    (w - border.horizontal() - padding.horizontal()).max(0.0),
                    (h - border.vertical() - padding.vertical()).max(0.0),
                )
            } else {
                (w, h)
            };
            let outer = Size::new(
                cw + padding.horizontal() + border.horizontal() + margin.horizontal(),
                ch + padding.vertical() + border.vertical() + margin.vertical(),
            );
            items.push(InlineItem {
                node: child,
                size: outer,
                // Atomic inlines sit on the baseline with their bottom margin edge.
                baseline: outer.height,
                atomic: Some(AtomicBox {
                    content: Size::new(cw, ch),
                    margin,
                    border,
                    padding,
                }),
                forced_break: false,
            });
        } else {
            // Inline-level element without an explicit size: approximate it
            // as a single glyph-sized box.
            let size = ctx.text_measure.measure_styled("X", style, max_width);
            let baseline = size.height * 0.8;

            items.push(InlineItem {
                node: child,
                size,
                baseline,
                atomic: None,
                forced_break: false,
            });
        }
    }
}

/// Calculate horizontal offset for text alignment.
fn calculate_text_align_offset(
    align: TextAlign,
    containing_width: f32,
    line_width: f32,
    _item_count: usize,
) -> f32 {
    match align {
        TextAlign::Left => 0.0,
        TextAlign::Right => (containing_width - line_width).max(0.0),
        TextAlign::Center => ((containing_width - line_width) / 2.0).max(0.0),
        TextAlign::Justify => {
            // Justify only applies to spacing between items, not initial offset
            0.0
        }
    }
}

/// Calculate vertical offset for an item based on vertical-align.
fn calculate_vertical_align_offset(
    align: VerticalAlign,
    line_baseline: f32,
    item_baseline: f32,
    item_height: f32,
    line_height: f32,
) -> f32 {
    match align {
        VerticalAlign::Baseline => line_baseline - item_baseline,
        VerticalAlign::Top => 0.0,
        VerticalAlign::Bottom => line_height - item_height,
        VerticalAlign::Middle => (line_height - item_height) / 2.0,
        VerticalAlign::Length(offset) => line_baseline - item_baseline + offset,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::{FixedWidthTextMeasure, TextMeasure};
    use crate::style::ComputedStyle;
    use crate::tree::{BoxTreeBuilder, NodeKind, TextContent};

    #[test]
    fn test_single_line_layout() {
        let mut builder = BoxTreeBuilder::new();
        let root = builder.root(ComputedStyle::block());
        builder.text(root, "Hello");
        let tree = builder.build();

        let ctx = LayoutContext::new(&tree, &FixedWidthTextMeasure, Size::new(800.0, 600.0));

        let mut fragments = Vec::new();
        let mut float_ctx = FloatContext::new(800.0);
        let height =
            layout_inline_formatting_context(&ctx, root, 800.0, &mut fragments, &mut float_ctx);

        assert_eq!(fragments.len(), 1); // One line box
        assert!(height > 0.0);
        assert_eq!(fragments[0].kind, FragmentKind::LineBox);
        assert_eq!(fragments[0].children.len(), 1); // One text run
    }

    #[test]
    fn test_line_wrapping() {
        let mut builder = BoxTreeBuilder::new();
        let root = builder.root(ComputedStyle::block());
        builder.text(root, "Hello");
        builder.text(root, "World");
        builder.text(root, "Test");
        let tree = builder.build();

        let ctx = LayoutContext::new(&tree, &FixedWidthTextMeasure, Size::new(800.0, 600.0));

        let mut fragments = Vec::new();
        let mut float_ctx = FloatContext::new(50.0);

        // Narrow width to force wrapping
        let height =
            layout_inline_formatting_context(&ctx, root, 50.0, &mut fragments, &mut float_ctx);

        // Should have multiple lines due to narrow width
        // Default line_height is 1.2 (unitless), so each line is 1.2px tall
        // With 3 text items wrapping to 3 lines: total height = 3.6px
        assert!(fragments.len() >= 2);
        assert!(height > 1.2); // More than one line height
    }

    #[test]
    fn test_text_align_center() {
        let mut builder = BoxTreeBuilder::new();
        let mut style = ComputedStyle::block();
        style.text_align = TextAlign::Center;
        let root = builder.root(style);
        builder.text(root, "Hi");
        let tree = builder.build();

        let ctx = LayoutContext::new(&tree, &FixedWidthTextMeasure, Size::new(800.0, 600.0));

        let mut fragments = Vec::new();
        let mut float_ctx = FloatContext::new(800.0);
        layout_inline_formatting_context(&ctx, root, 800.0, &mut fragments, &mut float_ctx);

        // Check that text is not at x=0 (it's centered)
        let line = &fragments[0];
        let text_run = &line.children[0];
        // "Hi" = 2 chars * 8px = 16px, centered in 800px = (800-16)/2 = 392
        assert!((text_run.position.x - 392.0).abs() < 1.0);
    }

    #[test]
    fn test_text_align_right() {
        let mut builder = BoxTreeBuilder::new();
        let mut style = ComputedStyle::block();
        style.text_align = TextAlign::Right;
        let root = builder.root(style);
        builder.text(root, "Hi");
        let tree = builder.build();

        let ctx = LayoutContext::new(&tree, &FixedWidthTextMeasure, Size::new(800.0, 600.0));

        let mut fragments = Vec::new();
        let mut float_ctx = FloatContext::new(800.0);
        layout_inline_formatting_context(&ctx, root, 800.0, &mut fragments, &mut float_ctx);

        // "Hi" = 2 chars * 8px = 16px, right-aligned in 800px = 800 - 16 = 784
        let line = &fragments[0];
        let text_run = &line.children[0];
        assert!((text_run.position.x - 784.0).abs() < 1.0);
    }

    #[test]
    fn test_text_align_justify() {
        let mut builder = BoxTreeBuilder::new();
        let mut style = ComputedStyle::block();
        style.text_align = TextAlign::Justify;
        let root = builder.root(style);
        builder.text(root, "A");
        builder.text(root, "B");
        // Forces a wrap: the first line is justified, the last line is not.
        builder.text(root, "CCCCCCCCCCCC");
        let tree = builder.build();

        let ctx = LayoutContext::new(&tree, &FixedWidthTextMeasure, Size::new(800.0, 600.0));

        let mut fragments = Vec::new();
        let mut float_ctx = FloatContext::new(100.0);
        layout_inline_formatting_context(&ctx, root, 100.0, &mut fragments, &mut float_ctx);

        assert_eq!(fragments.len(), 2);
        assert_eq!(fragments[1].children[0].position.x, 0.0);
        let line = &fragments[0];
        assert_eq!(line.children.len(), 2);

        // First item at x=0
        assert_eq!(line.children[0].position.x, 0.0);

        // Second item should have extra space added for justification
        // Each text is 8px wide, total 16px in 100px line
        // Extra space = 100 - 16 = 84px distributed across 1 gap
        // Second item x = 8 + 84 = 92
        assert!((line.children[1].position.x - 92.0).abs() < 1.0);
    }

    #[test]
    fn test_vertical_align_baseline() {
        let mut builder = BoxTreeBuilder::new();
        let root = builder.root(ComputedStyle::block());
        builder.text(root, "Test");
        let tree = builder.build();

        let ctx = LayoutContext::new(&tree, &FixedWidthTextMeasure, Size::new(800.0, 600.0));

        let mut fragments = Vec::new();
        let mut float_ctx = FloatContext::new(800.0);
        layout_inline_formatting_context(&ctx, root, 800.0, &mut fragments, &mut float_ctx);

        // Default is baseline alignment
        let line = &fragments[0];
        let text_run = &line.children[0];

        // Vertical offset should position text on baseline
        assert!(text_run.position.y >= 0.0);
    }

    #[test]
    fn test_vertical_align_top() {
        let mut builder = BoxTreeBuilder::new();
        let style = ComputedStyle::block();
        let root = builder.root(style);

        let mut text_style = ComputedStyle::inline();
        text_style.vertical_align = VerticalAlign::Top;

        // Create text with custom style
        let text_id = builder.tree.add_node(
            NodeKind::Text(TextContent {
                text: "Test".to_string(),
            }),
            text_style,
        );
        builder.tree.append_child(root, text_id);

        let tree = builder.build();

        let ctx = LayoutContext::new(&tree, &FixedWidthTextMeasure, Size::new(800.0, 600.0));

        let mut fragments = Vec::new();
        let mut float_ctx = FloatContext::new(800.0);
        layout_inline_formatting_context(&ctx, root, 800.0, &mut fragments, &mut float_ctx);

        let line = &fragments[0];
        let text_run = &line.children[0];

        // Top alignment means y = 0
        assert_eq!(text_run.position.y, 0.0);
    }

    #[test]
    fn test_empty_inline_context() {
        let mut builder = BoxTreeBuilder::new();
        let root = builder.root(ComputedStyle::block());
        let tree = builder.build();

        let ctx = LayoutContext::new(&tree, &FixedWidthTextMeasure, Size::new(800.0, 600.0));

        let mut fragments = Vec::new();
        let mut float_ctx = FloatContext::new(800.0);
        let height =
            layout_inline_formatting_context(&ctx, root, 800.0, &mut fragments, &mut float_ctx);

        assert_eq!(height, 0.0);
        assert_eq!(fragments.len(), 0);
    }

    #[test]
    fn test_white_space_nowrap() {
        let mut builder = BoxTreeBuilder::new();
        let mut style = ComputedStyle::block();
        style.white_space = WhiteSpace::Nowrap;
        let root = builder.root(style);
        builder.text(root, "A");
        builder.text(root, "B");
        builder.text(root, "C");
        let tree = builder.build();

        let ctx = LayoutContext::new(&tree, &FixedWidthTextMeasure, Size::new(800.0, 600.0));

        let mut fragments = Vec::new();
        let mut float_ctx = FloatContext::new(10.0);

        // Even with narrow width, nowrap should keep everything on one line
        layout_inline_formatting_context(&ctx, root, 10.0, &mut fragments, &mut float_ctx);

        // Should be one line despite narrow width
        assert_eq!(fragments.len(), 1);
        assert_eq!(fragments[0].children.len(), 3);
    }

    #[test]
    fn test_forced_line_break() {
        let mut builder = BoxTreeBuilder::new();
        let root = builder.root(ComputedStyle::block());
        builder.text(root, "A");
        builder.text(root, "\n");
        builder.text(root, "\n");
        builder.text(root, "B");
        let tree = builder.build();

        let ctx = LayoutContext::new(&tree, &FixedWidthTextMeasure, Size::new(800.0, 600.0));

        let mut fragments = Vec::new();
        let mut float_ctx = FloatContext::new(800.0);
        layout_inline_formatting_context(&ctx, root, 800.0, &mut fragments, &mut float_ctx);

        // "A", an empty line, then "B".
        assert_eq!(fragments.len(), 3);
        assert_eq!(fragments[1].children.len(), 0);
        assert!(fragments[2].position.y > fragments[1].position.y);
    }

    #[test]
    fn test_atomic_inline_box() {
        use crate::values::LengthPercentageAuto;
        let mut builder = BoxTreeBuilder::new();
        let root = builder.root(ComputedStyle::block());
        builder.text(root, "Hi");
        let mut img = ComputedStyle::inline();
        img.width = LengthPercentageAuto::px(40.0);
        img.height = LengthPercentageAuto::px(30.0);
        let img_id = builder.element(root, img);
        let tree = builder.build();

        let ctx = LayoutContext::new(&tree, &FixedWidthTextMeasure, Size::new(800.0, 600.0));

        let mut fragments = Vec::new();
        let mut float_ctx = FloatContext::new(800.0);
        let height =
            layout_inline_formatting_context(&ctx, root, 800.0, &mut fragments, &mut float_ctx);

        let line = &fragments[0];
        let atomic = line.children.iter().find(|f| f.node == img_id).unwrap();
        assert_eq!(atomic.kind, FragmentKind::Box);
        assert_eq!(atomic.size, Size::new(40.0, 30.0));
        assert_eq!(atomic.position.x, 16.0);
        // Line must hold the 30px box plus the text's descent below the baseline.
        assert!(height >= 30.0);
        let text = &line.children[0];
        assert!(text.position.y + text.size.height <= height + 0.01);
    }

    #[test]
    fn test_measure_styled_uses_font_size() {
        let mut style = ComputedStyle::inline();
        style.font_size = 20.0;
        assert_eq!(style.effective_font_size(), 20.0);
        let size = FixedWidthTextMeasure.measure_styled("ab", &style, 800.0);
        assert!((size.height - 24.0).abs() < 0.01);
    }

    #[test]
    fn test_text_flows_around_float() {
        use crate::layout::compute_layout;
        use crate::style::Float;
        use crate::values::LengthPercentageAuto;
        let mut builder = BoxTreeBuilder::new();
        let root = builder.root(ComputedStyle::block());
        let mut float_style = ComputedStyle::block();
        float_style.float = Float::Right;
        float_style.width = LengthPercentageAuto::px(100.0);
        float_style.height = LengthPercentageAuto::px(30.0);
        let fl = builder.element(root, float_style);
        let mut para_style = ComputedStyle::block();
        para_style.line_height = 10.0;
        let para = builder.element(root, para_style);
        for _ in 0..20 {
            builder.text(para, "word");
        }
        let tree = builder.build();
        let result = compute_layout(&tree, &FixedWidthTextMeasure, Size::new(200.0, 600.0));
        assert_eq!(result.bounding_rect(fl).unwrap().x, 100.0);
        let p = result.find_fragment(para).unwrap();
        // Lines beside the 30px float are 100px wide; later lines use 200px.
        assert!(p.children[0].size.width <= 100.0);
        let last = p.children.last().unwrap();
        assert!(last.position.y >= 30.0);
        assert_eq!(last.size.width, 200.0);
    }
}
