//! CSS Table Layout algorithm.
//!
//! Implements CSS 2.1 §17 — Tables.
//! Supports both `table-layout: fixed` and `table-layout: auto`.

use crate::box_model::BoxModel;
use crate::fragment::{Fragment, FragmentKind};
use crate::geometry::{Point, Size};
use crate::layout::{self, LayoutContext};
use crate::style::*;
use crate::tree::NodeId;
use crate::values::LengthPercentageAuto;

/// Layout a table element and its contents.
pub fn layout_table(
    ctx: &LayoutContext,
    node: NodeId,
    containing_block_width: f32,
    containing_block_height: f32,
) -> Fragment {
    let style = ctx.tree.style(node);
    let mut fragment = Fragment::new(node, FragmentKind::Box);

    // Resolve table box model
    let border = BoxModel::resolve_border(style);
    let padding = BoxModel::resolve_padding(style, containing_block_width);
    let margin = BoxModel::resolve_margin(style, containing_block_width);

    fragment.border = border;
    fragment.padding = padding;
    fragment.margin = margin;

    let explicit_width = style.width.resolve(containing_block_width).map(|mut w| {
        if style.box_sizing == BoxSizing::BorderBox {
            w = (w - border.horizontal() - padding.horizontal()).max(0.0);
        }
        w
    });
    let available_width =
        (containing_block_width - border.horizontal() - padding.horizontal() - margin.horizontal())
            .max(0.0);

    // Collect table structure
    let mut table = TableStructure::new();
    collect_table_structure(ctx, node, &mut table);

    let is_fixed = style.table_layout == TableLayout::Fixed;
    let is_collapse = style.border_collapse == BorderCollapse::Collapse;
    let border_spacing = if is_collapse {
        0.0
    } else {
        style.border_spacing
    };

    // Determine column widths
    let col_widths = if is_fixed {
        fixed_table_layout(
            &table,
            explicit_width.unwrap_or(available_width),
            border_spacing,
            ctx,
        )
    } else {
        auto_table_layout(&table, explicit_width, available_width, border_spacing, ctx)
    };

    let num_cols = col_widths.len();
    let total_spacing = border_spacing * (num_cols + 1) as f32;
    let table_content_width = col_widths.iter().sum::<f32>() + total_spacing;
    // An auto-width table is as wide as its columns (shrink-to-fit); a
    // fixed-layout table without a width fills the line, as before.
    let content_width = match explicit_width {
        Some(w) => w,
        None if is_fixed => available_width,
        None => table_content_width,
    };

    // Layout captions (top)
    let mut cursor_y = 0.0f32;
    if style.caption_side == CaptionSide::Top {
        for &caption_node in &table.captions {
            let mut cap_frag =
                layout::layout_node(ctx, caption_node, content_width, containing_block_height);
            cap_frag.position = Point::new(cap_frag.margin.left, cursor_y);
            cursor_y += cap_frag.border_box().height + cap_frag.margin.vertical();
            fragment.children.push(cap_frag);
        }
    }

    // Layout rows
    for row in table.rows.iter() {
        let mut row_height: f32 = 0.0;
        let mut cell_fragments: Vec<(usize, Fragment)> = Vec::new();

        for cell in &row.cells {
            let col_idx = cell.col_start;
            let col_span = cell.col_span;

            // Calculate cell width
            let mut cell_width = 0.0f32;
            for c in col_idx..(col_idx + col_span).min(num_cols) {
                cell_width += col_widths[c];
                if c > col_idx {
                    cell_width += border_spacing;
                }
            }

            // Layout cell content
            let cell_frag =
                layout::layout_node(ctx, cell.node, cell_width, containing_block_height);

            let cell_total_height = cell_frag.border_box().height;
            row_height = row_height.max(cell_total_height);

            cell_fragments.push((col_idx, cell_frag));
        }

        // Resolve specified row height
        if let Some(row_node) = row.node {
            let row_style = ctx.tree.style(row_node);
            if let Some(h) = row_style.height.resolve(containing_block_height) {
                row_height = row_height.max(h);
            }
        }

        // Position cells; every cell in a row is as tall as the row, so cell
        // backgrounds and borders line up.
        for (col_idx, mut cell_frag) in cell_fragments {
            let x = col_position(&col_widths, col_idx, border_spacing);
            cell_frag.position = Point::new(x, cursor_y);
            let extra = cell_frag.padding.vertical() + cell_frag.border.vertical();
            cell_frag.size.height = cell_frag.size.height.max(row_height - extra);

            // Vertical alignment in cell — default to top
            // TODO: implement vertical-align for table cells
            fragment.children.push(cell_frag);
        }

        cursor_y += row_height + border_spacing;
    }

    // Layout captions (bottom)
    if style.caption_side == CaptionSide::Bottom {
        for &caption_node in &table.captions {
            let mut cap_frag =
                layout::layout_node(ctx, caption_node, content_width, containing_block_height);
            cap_frag.position = Point::new(cap_frag.margin.left, cursor_y);
            cursor_y += cap_frag.border_box().height + cap_frag.margin.vertical();
            fragment.children.push(cap_frag);
        }
    }

    let final_height = style
        .height
        .resolve(containing_block_height)
        .unwrap_or(cursor_y);
    let min_h = style.min_height.resolve(containing_block_height);
    let max_h = style
        .max_height
        .resolve(containing_block_height)
        .unwrap_or(f32::INFINITY);

    fragment.size = Size::new(
        table_content_width.max(content_width),
        final_height.max(min_h).min(max_h),
    );

    fragment
}

// --- Table structure collection ---

struct TableStructure {
    rows: Vec<TableRow>,
    captions: Vec<NodeId>,
    column_specs: Vec<ColumnSpec>,
}

struct TableRow {
    node: Option<NodeId>,
    cells: Vec<TableCell>,
}

struct TableCell {
    node: NodeId,
    col_start: usize,
    col_span: usize,
}

struct ColumnSpec {
    width: Option<f32>,
}

impl TableStructure {
    fn new() -> Self {
        Self {
            rows: Vec::new(),
            captions: Vec::new(),
            column_specs: Vec::new(),
        }
    }

    fn num_columns(&self) -> usize {
        let from_cells = self
            .rows
            .iter()
            .flat_map(|r| &r.cells)
            .map(|c| c.col_start + c.col_span)
            .max()
            .unwrap_or(0);
        from_cells.max(self.column_specs.len())
    }
}

fn collect_table_structure(ctx: &LayoutContext, table_node: NodeId, table: &mut TableStructure) {
    let children = ctx.tree.children(table_node);

    for &child_id in children {
        let child_style = ctx.tree.style(child_id);

        match child_style.display.inner {
            DisplayInner::TableCaption => {
                table.captions.push(child_id);
            }
            DisplayInner::TableRow => {
                collect_row(ctx, child_id, table);
            }
            DisplayInner::TableRowGroup
            | DisplayInner::TableHeaderGroup
            | DisplayInner::TableFooterGroup => {
                // Recurse into row groups
                let group_children = ctx.tree.children(child_id);
                for &gc in group_children {
                    let gc_style = ctx.tree.style(gc);
                    if gc_style.display.inner == DisplayInner::TableRow {
                        collect_row(ctx, gc, table);
                    }
                }
            }
            DisplayInner::TableColumn | DisplayInner::TableColumnGroup => {
                // Column definitions (for width hints)
                let col_style = ctx.tree.style(child_id);
                if let Some(w) = col_style.width.resolve(0.0) {
                    table.column_specs.push(ColumnSpec { width: Some(w) });
                }
            }
            DisplayInner::TableCell => {
                // Direct cell child (implicit row)
                let row = TableRow {
                    node: None,
                    cells: vec![TableCell {
                        node: child_id,
                        col_start: 0,
                        col_span: 1,
                    }],
                };
                table.rows.push(row);
            }
            _ => {
                // Treat as anonymous table cell
            }
        }
    }
}

fn collect_row(ctx: &LayoutContext, row_node: NodeId, table: &mut TableStructure) {
    let children = ctx.tree.children(row_node);
    let mut cells = Vec::new();
    let mut col = 0;

    for &child_id in children {
        let child_style = ctx.tree.style(child_id);
        if child_style.display.is_none() {
            continue;
        }

        cells.push(TableCell {
            node: child_id,
            col_start: col,
            col_span: 1, // TODO: colspan attribute support
        });
        col += 1;
    }

    table.rows.push(TableRow {
        node: Some(row_node),
        cells,
    });
}

// --- Fixed table layout (CSS 2.1 §17.5.2.1) ---

fn fixed_table_layout(
    table: &TableStructure,
    table_width: f32,
    border_spacing: f32,
    ctx: &LayoutContext,
) -> Vec<f32> {
    let num_cols = table.num_columns().max(1);
    let total_spacing = border_spacing * (num_cols + 1) as f32;
    let available = (table_width - total_spacing).max(0.0);

    let mut widths = vec![0.0f32; num_cols];
    let mut assigned = vec![false; num_cols];

    // First: use column specs
    for (i, spec) in table.column_specs.iter().enumerate() {
        if i < num_cols {
            if let Some(w) = spec.width {
                widths[i] = w;
                assigned[i] = true;
            }
        }
    }

    // Second: use first row cell widths
    if let Some(first_row) = table.rows.first() {
        for cell in &first_row.cells {
            if cell.col_start < num_cols && !assigned[cell.col_start] {
                let cell_style = ctx.tree.style(cell.node);
                if let Some(w) = cell_style.width.resolve(available) {
                    widths[cell.col_start] = w;
                    assigned[cell.col_start] = true;
                }
            }
        }
    }

    // Third: distribute remaining space equally to unassigned columns
    let assigned_total: f32 = widths.iter().sum();
    let remaining = (available - assigned_total).max(0.0);
    let unassigned_count = assigned.iter().filter(|&&a| !a).count();

    if unassigned_count > 0 {
        let per_col = remaining / unassigned_count as f32;
        for i in 0..num_cols {
            if !assigned[i] {
                widths[i] = per_col;
            }
        }
    }

    widths
}

// --- Auto table layout (CSS 2.1 §17.5.2.2) ---

/// Per-column (min-content, max-content) widths and the border spacing.
pub(crate) fn column_intrinsics(
    ctx: &LayoutContext,
    table_node: NodeId,
) -> (Vec<f32>, Vec<f32>, f32) {
    let style = ctx.tree.style(table_node);
    let spacing = if style.border_collapse == BorderCollapse::Collapse {
        0.0
    } else {
        style.border_spacing
    };
    let mut table = TableStructure::new();
    collect_table_structure(ctx, table_node, &mut table);
    let (mins, maxs, _) = column_widths(&table, ctx, f32::INFINITY);
    (mins, maxs, spacing)
}

/// Column min/max widths from cell contents (CSS 2.1 §17.5.2.2), plus which
/// columns have an explicit width.
fn column_widths(
    table: &TableStructure,
    ctx: &LayoutContext,
    available: f32,
) -> (Vec<f32>, Vec<f32>, Vec<bool>) {
    let n = table.num_columns().max(1);
    let mut mins = vec![0.0f32; n];
    let mut maxs = vec![0.0f32; n];
    let mut fixed = vec![false; n];
    for row in &table.rows {
        for cell in &row.cells {
            if cell.col_start >= n {
                continue;
            }
            let (mut mn, mut mx) = crate::intrinsic::intrinsic_widths(ctx, cell.node);
            let cell_style = ctx.tree.style(cell.node);
            if let LengthPercentageAuto::Percentage(p) = cell_style.width {
                if available.is_finite() {
                    let w = available * p;
                    mx = mx.max(w);
                    mn = mn.min(mx);
                }
            }
            if matches!(cell_style.width, LengthPercentageAuto::Length(_)) {
                fixed[cell.col_start] = true;
            }
            let span = cell.col_span.max(1).min(n - cell.col_start);
            if span == 1 {
                mins[cell.col_start] = mins[cell.col_start].max(mn);
                maxs[cell.col_start] = maxs[cell.col_start].max(mx);
            } else {
                // Spread whatever the spanned columns lack evenly.
                let cols = cell.col_start..cell.col_start + span;
                let have_min: f32 = mins[cols.clone()].iter().sum();
                let have_max: f32 = maxs[cols.clone()].iter().sum();
                for c in cols {
                    mins[c] += (mn - have_min).max(0.0) / span as f32;
                    maxs[c] += (mx - have_max).max(0.0) / span as f32;
                }
            }
        }
    }
    for (i, spec) in table.column_specs.iter().enumerate() {
        if let (Some(w), true) = (spec.width, i < n) {
            mins[i] = mins[i].max(w);
            maxs[i] = maxs[i].max(w);
            fixed[i] = true;
        }
    }
    for i in 0..n {
        maxs[i] = maxs[i].max(mins[i]);
    }
    (mins, maxs, fixed)
}

/// Automatic table layout: columns get their max-content width when it fits,
/// their min-content width when even that does not, and in between
/// proportionally otherwise. A table with an explicit width shares out any
/// extra space among columns without an explicit width.
fn auto_table_layout(
    table: &TableStructure,
    explicit_width: Option<f32>,
    available_width: f32,
    border_spacing: f32,
    ctx: &LayoutContext,
) -> Vec<f32> {
    let num_cols = table.num_columns().max(1);
    let total_spacing = border_spacing * (num_cols + 1) as f32;
    let (mins, maxs, fixed) = column_widths(table, ctx, available_width - total_spacing);
    let sum_min: f32 = mins.iter().sum();
    let sum_max: f32 = maxs.iter().sum();
    let target = explicit_width
        .unwrap_or(available_width)
        .min(if explicit_width.is_some() {
            f32::INFINITY
        } else {
            available_width
        })
        - total_spacing;
    let target = target.max(0.0);

    if sum_max <= target {
        let mut widths = maxs.clone();
        if explicit_width.is_some() {
            // Distribute the surplus, preferring columns without a set width.
            let extra = target - sum_max;
            let flexible: Vec<usize> = (0..num_cols).filter(|&i| !fixed[i]).collect();
            let pool: Vec<usize> = if flexible.is_empty() {
                (0..num_cols).collect()
            } else {
                flexible
            };
            let weight: f32 = pool.iter().map(|&i| maxs[i]).sum();
            for &i in &pool {
                widths[i] += if weight > 0.0 {
                    extra * maxs[i] / weight
                } else {
                    extra / pool.len() as f32
                };
            }
        }
        widths
    } else if sum_min >= target {
        mins
    } else {
        let range = sum_max - sum_min;
        let factor = if range > 0.0 {
            (target - sum_min) / range
        } else {
            0.0
        };
        (0..num_cols)
            .map(|i| mins[i] + (maxs[i] - mins[i]) * factor)
            .collect()
    }
}

fn col_position(col_widths: &[f32], col_idx: usize, border_spacing: f32) -> f32 {
    let mut x = border_spacing;
    for i in 0..col_idx {
        x += col_widths[i] + border_spacing;
    }
    x
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::{compute_layout, FixedWidthTextMeasure, TextMeasure};
    use crate::style::ComputedStyle;
    use crate::tree::BoxTreeBuilder;
    use crate::values::LengthPercentageAuto;

    fn make_table_style() -> ComputedStyle {
        ComputedStyle {
            display: Display::TABLE,
            ..ComputedStyle::block()
        }
    }

    fn make_row_style() -> ComputedStyle {
        ComputedStyle {
            display: Display::TABLE_ROW,
            ..ComputedStyle::block()
        }
    }

    fn make_cell_style() -> ComputedStyle {
        let mut s = ComputedStyle {
            display: Display::TABLE_CELL,
            ..ComputedStyle::block()
        };
        s.height = LengthPercentageAuto::px(30.0);
        s
    }

    #[test]
    fn test_simple_table_2x2() {
        let mut builder = BoxTreeBuilder::new();
        let root = builder.root(make_table_style());

        let row1 = builder.element(root, make_row_style());
        builder.element(row1, make_cell_style());
        builder.element(row1, make_cell_style());

        let row2 = builder.element(root, make_row_style());
        builder.element(row2, make_cell_style());
        builder.element(row2, make_cell_style());

        let tree = builder.build();
        let result = compute_layout(&tree, &FixedWidthTextMeasure, Size::new(800.0, 600.0));

        let root_layout = result.bounding_rect(tree.root()).unwrap();
        // Empty cells: an auto-width table shrinks to its (empty) content.
        assert!(root_layout.width < 800.0);
        assert!(root_layout.height > 0.0);
    }

    fn r_lines(result: &crate::fragment::LayoutResult, node: NodeId) -> usize {
        result
            .find_fragment(node)
            .map(|f| {
                f.children
                    .iter()
                    .filter(|c| c.kind == FragmentKind::LineBox)
                    .count()
            })
            .unwrap_or(0)
    }

    fn text_cell(b: &mut BoxTreeBuilder, row: NodeId, words: &[&str]) -> NodeId {
        let mut s = make_cell_style();
        s.height = LengthPercentageAuto::Auto;
        let c = b.element(row, s);
        for w in words {
            b.text(c, w);
        }
        c
    }

    #[test]
    fn test_auto_columns_follow_content() {
        // A results-style table: a narrow number column beside long text.
        let mut b = BoxTreeBuilder::new();
        let root = b.root(ComputedStyle::block());
        let table = b.element(root, make_table_style());
        let row = b.element(table, make_row_style());
        let num = text_cell(&mut b, row, &["1."]);
        let long: Vec<&str> = std::iter::repeat_n("lorem ", 60).collect();
        let text = text_cell(&mut b, row, &long);
        let tree = b.build();
        let result = compute_layout(&tree, &FixedWidthTextMeasure, Size::new(800.0, 600.0));
        let n = result.bounding_rect(num).unwrap();
        let t = result.bounding_rect(text).unwrap();
        // "1." is 16px; the text column takes the rest of the 800px line.
        assert!(n.width < 30.0, "number column {}", n.width);
        assert!(t.width > 700.0, "text column {}", t.width);
        // The long text wraps onto several lines inside its column, while the
        // number stays on one (the row stretches both cells to its height).
        let line = crate::layout::FixedWidthTextMeasure
            .measure_styled("1.", tree.style(num), 800.0)
            .height;
        let lines = r_lines(&result, text);
        assert!(lines >= 3, "lines {lines}");
        assert!(t.height >= line * 3.0);
    }

    #[test]
    fn test_auto_table_shrinks_and_fills() {
        let build = |width: Option<f32>| {
            let mut b = BoxTreeBuilder::new();
            let root = b.root(ComputedStyle::block());
            let mut ts = make_table_style();
            if let Some(p) = width {
                ts.width = LengthPercentageAuto::percent(p);
            }
            let table = b.element(root, ts);
            let row = b.element(table, make_row_style());
            let a = text_cell(&mut b, row, &["aaaa"]);
            let c = text_cell(&mut b, row, &["bbbbbbbb"]);
            (b.build(), table, a, c)
        };
        let (tree, table, a, c) = build(None);
        let r = compute_layout(&tree, &FixedWidthTextMeasure, Size::new(800.0, 600.0));
        // Shrink-to-fit: 32px + 64px of content.
        assert_eq!(r.bounding_rect(table).unwrap().width, 96.0);
        assert_eq!(r.bounding_rect(a).unwrap().width, 32.0);
        assert_eq!(r.bounding_rect(c).unwrap().width, 64.0);

        let (tree, table, a, c) = build(Some(100.0));
        let r = compute_layout(&tree, &FixedWidthTextMeasure, Size::new(800.0, 600.0));
        assert!((r.bounding_rect(table).unwrap().width - 800.0).abs() < 0.01);
        // Extra space is shared in proportion to content (1:2).
        let (wa, wc) = (
            r.bounding_rect(a).unwrap().width,
            r.bounding_rect(c).unwrap().width,
        );
        assert!((wc / wa - 2.0).abs() < 0.01, "{wa} {wc}");
    }

    #[test]
    fn test_col_position() {
        let widths = vec![100.0, 200.0, 150.0];
        assert_eq!(col_position(&widths, 0, 5.0), 5.0);
        assert_eq!(col_position(&widths, 1, 5.0), 110.0); // 5 + 100 + 5
        assert_eq!(col_position(&widths, 2, 5.0), 315.0); // 5 + 100 + 5 + 200 + 5
    }
}
