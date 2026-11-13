//! Layout entry point and dispatch.

use crate::block;
use crate::flex;
use crate::fragment::{Fragment, FragmentKind, LayoutResult};
use crate::geometry::{Rect, Size};
use crate::grid;
use crate::position;
use crate::style::ComputedStyle;
use crate::style::Float;
use crate::table;
use crate::tree::{BoxTree, FormattingContextType, NodeId};

/// Text measurement callback.
pub trait TextMeasure {
    /// Measure the width and height of a text string within a maximum width.
    fn measure(&self, text: &str, font_size: f32, max_width: f32) -> Size;

    /// Measure text using the full computed style of the text node.
    ///
    /// Embedders that need more than a font size (family, weight, letter
    /// spacing, ...) can override this and read [`ComputedStyle::font_id`].
    /// The default implementation forwards to [`TextMeasure::measure`].
    fn measure_styled(&self, text: &str, style: &ComputedStyle, max_width: f32) -> Size {
        self.measure(text, style.effective_font_size(), max_width)
    }
}

/// A fixed-width text measurer for testing (every character = 8px wide).
pub struct FixedWidthTextMeasure;

impl TextMeasure for FixedWidthTextMeasure {
    fn measure(&self, text: &str, font_size: f32, max_width: f32) -> Size {
        let char_width = 8.0;
        let line_height = font_size * 1.2;

        if text.is_empty() {
            return Size::new(0.0, line_height);
        }

        let chars_per_line = (max_width / char_width).floor().max(1.0) as usize;
        let words: Vec<&str> = text.split_whitespace().collect();

        if words.is_empty() {
            return Size::new(0.0, line_height);
        }

        let mut lines = 1usize;
        let mut current_line_chars = 0usize;
        let mut max_line_width = 0.0f32;

        for word in words.iter() {
            let word_chars = word.len();
            let needed = if current_line_chars > 0 {
                word_chars + 1 // space before word
            } else {
                word_chars
            };

            if current_line_chars > 0 && current_line_chars + needed > chars_per_line {
                // Wrap to new line
                let line_width = current_line_chars as f32 * char_width;
                max_line_width = max_line_width.max(line_width);
                lines += 1;
                current_line_chars = word_chars;
            } else {
                if current_line_chars > 0 {
                    current_line_chars += 1; // space
                }
                current_line_chars += word_chars;
            }
        }

        // Final line
        let line_width = current_line_chars as f32 * char_width;
        max_line_width = max_line_width.max(line_width);

        Size::new(max_line_width, lines as f32 * line_height)
    }
}

/// Float exclusions in some box's coordinate space.
pub(crate) type FloatList = Vec<(Rect, Float)>;

/// Layout context passed through the layout tree.
pub struct LayoutContext<'a> {
    pub tree: &'a BoxTree,
    pub text_measure: &'a dyn TextMeasure,
    pub viewport: Size,
    /// Border-box heights imposed on boxes by their flex container (stretched
    /// cross size, or resolved main size in a column). Consumed when the box
    /// is laid out.
    forced_heights: core::cell::RefCell<Vec<(NodeId, f32)>>,
    /// Float exclusions handed from a block to a child that shares its
    /// block formatting context, in the child's content-box coordinates.
    inherited_floats: core::cell::RefCell<Vec<(NodeId, FloatList)>>,
}

impl<'a> LayoutContext<'a> {
    pub fn new(tree: &'a BoxTree, text_measure: &'a dyn TextMeasure, viewport: Size) -> Self {
        Self {
            tree,
            text_measure,
            viewport,
            forced_heights: core::cell::RefCell::new(Vec::new()),
            inherited_floats: core::cell::RefCell::new(Vec::new()),
        }
    }

    pub(crate) fn pass_floats(&self, node: NodeId, floats: FloatList) {
        let mut v = self.inherited_floats.borrow_mut();
        v.retain(|(n, _)| *n != node);
        v.push((node, floats));
    }

    pub(crate) fn take_floats(&self, node: NodeId) -> FloatList {
        let mut v = self.inherited_floats.borrow_mut();
        match v.iter().position(|(n, _)| *n == node) {
            Some(i) => v.swap_remove(i).1,
            None => Vec::new(),
        }
    }

    /// Impose a border-box height on `node` for its next layout.
    pub(crate) fn force_height(&self, node: NodeId, height: f32) {
        let mut v = self.forced_heights.borrow_mut();
        v.retain(|(n, _)| *n != node);
        v.push((node, height.max(0.0)));
    }

    /// Take (and clear) the height imposed on `node`, if any.
    pub(crate) fn take_forced_height(&self, node: NodeId) -> Option<f32> {
        let mut v = self.forced_heights.borrow_mut();
        let pos = v.iter().position(|(n, _)| *n == node)?;
        Some(v.swap_remove(pos).1)
    }
}

/// Compute layout for an entire box tree.
pub fn compute_layout(
    tree: &BoxTree,
    text_measure: &dyn TextMeasure,
    viewport: Size,
) -> LayoutResult {
    let ctx = LayoutContext::new(tree, text_measure, viewport);

    let root = tree.root();
    let root_fragment = layout_node(&ctx, root, viewport.width, viewport.height);

    // After main layout, resolve absolute/fixed positioned elements
    let root_fragment = position::resolve_positioned(tree, root_fragment, viewport);

    LayoutResult {
        root: root_fragment,
    }
}

/// Layout a single node and its subtree.
pub fn layout_node(
    ctx: &LayoutContext,
    node: NodeId,
    containing_block_width: f32,
    containing_block_height: f32,
) -> Fragment {
    let style = ctx.tree.style(node);

    // Skip display: none
    if style.display.is_none() {
        let mut f = Fragment::new(node, FragmentKind::Box);
        f.size = Size::ZERO;
        return f;
    }

    // Text nodes
    if let Some(text) = ctx.tree.node(node).text_content() {
        return layout_text(ctx, node, text, containing_block_width);
    }

    // Determine formatting context for children
    let fc = ctx.tree.formatting_context(node);

    match fc {
        FormattingContextType::Block => {
            block::layout_block(ctx, node, containing_block_width, containing_block_height)
        }
        FormattingContextType::Inline => {
            block::layout_block(ctx, node, containing_block_width, containing_block_height)
        }
        FormattingContextType::Flex => {
            flex::layout_flex(ctx, node, containing_block_width, containing_block_height)
        }
        FormattingContextType::Grid => {
            grid::layout_grid(ctx, node, containing_block_width, containing_block_height)
        }
        FormattingContextType::Table => {
            table::layout_table(ctx, node, containing_block_width, containing_block_height)
        }
    }
}

/// Layout a text node.
fn layout_text(ctx: &LayoutContext, node: NodeId, text: &str, max_width: f32) -> Fragment {
    let style = ctx.tree.style(node);
    let size = ctx.text_measure.measure_styled(text, style, max_width);

    let mut fragment = Fragment::new(node, FragmentKind::TextRun);
    fragment.size = size;
    fragment
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::style::ComputedStyle;
    use crate::tree::BoxTreeBuilder;
    use crate::values::LengthPercentageAuto;

    #[test]
    fn test_simple_block_layout() {
        let mut builder = BoxTreeBuilder::new();
        let root = builder.root(ComputedStyle::block());
        let mut child_style = ComputedStyle::block();
        child_style.height = LengthPercentageAuto::px(100.0);
        builder.element(root, child_style);

        let tree = builder.build();
        let result = compute_layout(&tree, &FixedWidthTextMeasure, Size::new(800.0, 600.0));

        let root_rect = result.bounding_rect(tree.root()).unwrap();
        assert_eq!(root_rect.width, 800.0);
        assert_eq!(root_rect.height, 100.0);
    }

    #[test]
    fn test_two_blocks_stack_vertically() {
        let mut builder = BoxTreeBuilder::new();
        let root = builder.root(ComputedStyle::block());

        let mut child1_style = ComputedStyle::block();
        child1_style.height = LengthPercentageAuto::px(50.0);
        let c1 = builder.element(root, child1_style);

        let mut child2_style = ComputedStyle::block();
        child2_style.height = LengthPercentageAuto::px(75.0);
        let c2 = builder.element(root, child2_style);

        let tree = builder.build();
        let result = compute_layout(&tree, &FixedWidthTextMeasure, Size::new(800.0, 600.0));

        let root_rect = result.bounding_rect(tree.root()).unwrap();
        assert_eq!(root_rect.height, 125.0); // 50 + 75

        let c1_rect = result.bounding_rect(c1).unwrap();
        assert_eq!(c1_rect.y, 0.0);
        assert_eq!(c1_rect.height, 50.0);

        let c2_rect = result.bounding_rect(c2).unwrap();
        assert_eq!(c2_rect.y, 50.0);
        assert_eq!(c2_rect.height, 75.0);
    }

    #[test]
    fn test_fixed_width_text_measure() {
        let m = FixedWidthTextMeasure;
        let size = m.measure("Hello World", 16.0, 800.0);
        assert_eq!(size.width, 88.0); // 11 chars * 8px
        assert!((size.height - 19.2).abs() < 0.01); // 16 * 1.2
    }
}
