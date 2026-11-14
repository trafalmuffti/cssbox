//! CSS Grid Layout algorithm.
//!
//! Implements CSS Grid Layout Module Level 2.
//! Reference: https://www.w3.org/TR/css-grid-2/

use crate::box_model::BoxModel;
use crate::fragment::{Fragment, FragmentKind};
use crate::geometry::{Point, Size};
use crate::layout::{self, LayoutContext};
use crate::style::*;
use crate::tree::NodeId;

/// Layout a grid container and its items.
pub fn layout_grid(
    ctx: &LayoutContext,
    node: NodeId,
    containing_block_width: f32,
    containing_block_height: f32,
) -> Fragment {
    let style = ctx.tree.style(node);
    let mut fragment = Fragment::new(node, FragmentKind::Box);

    // Resolve container box model
    let border = BoxModel::resolve_border(style);
    let padding = BoxModel::resolve_padding(style, containing_block_width);
    let margin = BoxModel::resolve_margin(style, containing_block_width);

    fragment.border = border;
    fragment.padding = padding;
    fragment.margin = margin;

    let content_width = match style.width.resolve(containing_block_width) {
        Some(mut w) => {
            if style.box_sizing == BoxSizing::BorderBox {
                w = (w - border.horizontal() - padding.horizontal()).max(0.0);
            }
            w
        }
        None => (containing_block_width
            - border.horizontal()
            - padding.horizontal()
            - margin.horizontal())
        .max(0.0),
    };

    // A definite content height: specified, or imposed by a parent flex container.
    let forced = ctx.take_forced_height(node);
    let definite_height = match style.height.resolve(containing_block_height) {
        Some(mut h) => {
            if style.box_sizing == BoxSizing::BorderBox {
                h = (h - border.vertical() - padding.vertical()).max(0.0);
            }
            Some(h)
        }
        None => forced.map(|h| (h - border.vertical() - padding.vertical()).max(0.0)),
    };

    // §7: Define the explicit grid
    let explicit_cols = &style.grid_template_columns;
    let explicit_rows = &style.grid_template_rows;
    let col_gap = style.column_gap;
    let row_gap = style.row_gap;

    // Collect grid items
    let children = ctx.tree.children(node);
    let mut items: Vec<GridItem> = Vec::new();

    for &child_id in children {
        let child_style = ctx.tree.style(child_id);
        if child_style.display.is_none() {
            continue;
        }
        if child_style.position.is_absolutely_positioned() {
            let child_frag = layout::layout_node(
                ctx,
                child_id,
                content_width,
                definite_height.unwrap_or(containing_block_height),
            );
            fragment.children.push(child_frag);
            continue;
        }
        items.push(GridItem {
            node: child_id,
            row: Axis::from(
                &child_style.grid_row_start,
                &child_style.grid_row_end,
                explicit_rows.len(),
            ),
            col: Axis::from(
                &child_style.grid_column_start,
                &child_style.grid_column_end,
                explicit_cols.len(),
            ),
            row_start: 0,
            col_start: 0,
            row_span: 1,
            col_span: 1,
        });
    }
    // `order` also orders grid items.
    items.sort_by_key(|i| ctx.tree.style(i.node).order);

    // §8: Place items onto the grid
    let n_cols = explicit_cols.len().max(1);
    let (grid_col_count, grid_row_count) =
        place_items(&mut items, n_cols, explicit_rows.len(), style);

    // §12: Track sizing. Columns first (items' widths feed rows' heights).
    let col_sizes = size_tracks(
        explicit_cols,
        &style.grid_auto_columns,
        grid_col_count,
        Some(content_width),
        col_gap,
        &items,
        ctx,
        true,
        &[],
        content_width,
        containing_block_height,
    );
    let row_sizes = size_tracks(
        explicit_rows,
        &style.grid_auto_rows,
        grid_row_count,
        definite_height,
        row_gap,
        &items,
        ctx,
        false,
        &col_sizes,
        content_width,
        containing_block_height,
    );
    let col_gap_between = col_gap;

    // Compute track positions
    let col_positions = track_positions(&col_sizes, col_gap_between);
    let row_positions = track_positions(&row_sizes, row_gap);

    let total_row_size = match (row_positions.last(), row_sizes.last()) {
        (Some(p), Some(s)) => p + s,
        _ => 0.0,
    };

    // Layout and position each item
    for item in &items {
        let cs = item.col_start;
        let ce = (cs + item.col_span).min(col_sizes.len());
        let rs = item.row_start;
        let re = (rs + item.row_span).min(row_sizes.len());

        let x = col_positions.get(cs).copied().unwrap_or(0.0);
        let y = row_positions.get(rs).copied().unwrap_or(0.0);
        let item_width = span_size(&col_sizes, cs, ce, col_gap_between);
        let item_height = span_size(&row_sizes, rs, re, row_gap);

        let child_style = ctx.tree.style(item.node);
        let align = effective_align_items(style.align_items, child_style.align_self);
        let item_box = BoxModel::resolve(child_style, item_width);
        // Items with an auto height stretch to their area.
        if matches!(align, AlignItems::Stretch) && child_style.height.is_auto() {
            ctx.force_height(
                item.node,
                (item_height - item_box.margin.vertical()).max(0.0),
            );
        }
        let mut child_frag = layout::layout_node(ctx, item.node, item_width, item_height);

        let justify = style.justify_content;
        let actual_w = child_frag.border_box().width + child_frag.margin.horizontal();
        let actual_h = child_frag.border_box().height + child_frag.margin.vertical();

        let dx = match justify {
            JustifyContent::Center => (item_width - actual_w) / 2.0,
            JustifyContent::End | JustifyContent::FlexEnd => item_width - actual_w,
            _ => 0.0,
        };
        let dy = match align {
            AlignItems::Center => (item_height - actual_h) / 2.0,
            AlignItems::End | AlignItems::FlexEnd => item_height - actual_h,
            _ => 0.0,
        };

        child_frag.position = Point::new(
            x + dx + child_frag.margin.left,
            y + dy + child_frag.margin.top,
        );
        fragment.children.push(child_frag);
    }

    let final_height = definite_height.unwrap_or(total_row_size);
    let min_h = style.min_height.resolve(containing_block_height);
    let max_h = style
        .max_height
        .resolve(containing_block_height)
        .unwrap_or(f32::INFINITY);

    fragment.size = Size::new(content_width, final_height.max(min_h).min(max_h));
    fragment
}

fn span_size(sizes: &[f32], from: usize, to: usize, gap: f32) -> f32 {
    let mut total = 0.0;
    for (i, s) in sizes.iter().enumerate().take(to).skip(from) {
        total += s;
        if i > from {
            total += gap;
        }
    }
    total
}

/// One axis of an item's placement before auto-placement.
#[derive(Clone, Copy, Debug)]
struct Axis {
    /// 0-based start line, if definite.
    start: Option<usize>,
    /// Number of tracks spanned.
    span: usize,
}

impl Axis {
    /// Resolve `grid-*-start` / `grid-*-end` for an explicit grid of `n` tracks.
    fn from(start: &GridPlacement, end: &GridPlacement, explicit: usize) -> Axis {
        let line = |n: i32| -> Option<usize> {
            if n > 0 {
                Some((n - 1) as usize)
            } else if n < 0 {
                let idx = explicit as i32 + 1 + n;
                (idx >= 0).then_some(idx as usize)
            } else {
                None
            }
        };
        match (start, end) {
            (GridPlacement::Line(a), GridPlacement::Line(b)) => match (line(*a), line(*b)) {
                (Some(a), Some(b)) => {
                    let (lo, hi) = if a <= b { (a, b) } else { (b, a) };
                    Axis {
                        start: Some(lo),
                        span: (hi - lo).max(1),
                    }
                }
                (Some(a), None) => Axis {
                    start: Some(a),
                    span: 1,
                },
                _ => Axis {
                    start: None,
                    span: 1,
                },
            },
            (GridPlacement::Line(a), GridPlacement::Span(k)) => Axis {
                start: line(*a),
                span: (*k as usize).max(1),
            },
            (GridPlacement::Line(a), _) => Axis {
                start: line(*a),
                span: 1,
            },
            (GridPlacement::Span(k), GridPlacement::Line(b)) => match line(*b) {
                Some(b) => {
                    let span = (*k as usize).max(1);
                    Axis {
                        start: Some(b.saturating_sub(span)),
                        span,
                    }
                }
                None => Axis {
                    start: None,
                    span: (*k as usize).max(1),
                },
            },
            (GridPlacement::Span(k), _) => Axis {
                start: None,
                span: (*k as usize).max(1),
            },
            (_, GridPlacement::Line(b)) => match line(*b) {
                Some(b) if b > 0 => Axis {
                    start: Some(b - 1),
                    span: 1,
                },
                _ => Axis {
                    start: None,
                    span: 1,
                },
            },
            (_, GridPlacement::Span(k)) => Axis {
                start: None,
                span: (*k as usize).max(1),
            },
            _ => Axis {
                start: None,
                span: 1,
            },
        }
    }
}

struct GridItem {
    node: NodeId,
    row: Axis,
    col: Axis,
    row_start: usize,
    col_start: usize,
    row_span: usize,
    col_span: usize,
}

/// §8.5 auto-placement (sparse and dense, row or column major) with an
/// occupancy map. Returns the number of columns and rows of the final grid.
fn place_items(
    items: &mut [GridItem],
    n_cols: usize,
    n_explicit_rows: usize,
    style: &ComputedStyle,
) -> (usize, usize) {
    use std::collections::HashSet;
    let column_flow = matches!(
        style.grid_auto_flow,
        GridAutoFlow::Column | GridAutoFlow::ColumnDense
    );
    let dense = matches!(
        style.grid_auto_flow,
        GridAutoFlow::RowDense | GridAutoFlow::ColumnDense
    );
    let mut cols = n_cols;
    let mut rows = n_explicit_rows.max(1);
    let mut taken: HashSet<(usize, usize)> = HashSet::new();
    let free = |taken: &HashSet<(usize, usize)>, r: usize, c: usize, rs: usize, cs: usize| {
        (r..r + rs).all(|rr| (c..c + cs).all(|cc| !taken.contains(&(rr, cc))))
    };
    let mark = |taken: &mut HashSet<(usize, usize)>, r: usize, c: usize, rs: usize, cs: usize| {
        for rr in r..r + rs {
            for cc in c..c + cs {
                taken.insert((rr, cc));
            }
        }
    };

    // Items with both axes definite go first.
    for it in items.iter_mut() {
        it.row_span = it.row.span;
        it.col_span = it.col.span;
        if let (Some(r), Some(c)) = (it.row.start, it.col.start) {
            it.row_start = r;
            it.col_start = c;
            mark(&mut taken, r, c, it.row_span, it.col_span);
            cols = cols.max(c + it.col_span);
            rows = rows.max(r + it.row_span);
        }
    }
    // Sparse cursor.
    let (mut cur_r, mut cur_c) = (0usize, 0usize);
    for it in items.iter_mut() {
        if it.row.start.is_some() && it.col.start.is_some() {
            continue;
        }
        let (rs, cs) = (it.row_span, it.col_span);
        if column_flow {
            // Fill columns top to bottom within the explicit rows.
            let limit = rows.max(rs);
            let mut c = it.col.start.unwrap_or(if dense { 0 } else { cur_c });
            let mut r = it.row.start.unwrap_or(if dense || it.col.start.is_some() {
                0
            } else {
                cur_r
            });
            loop {
                if r + rs > limit && it.row.start.is_none() {
                    r = 0;
                    c += 1;
                    continue;
                }
                if free(&taken, r, c, rs, cs) {
                    break;
                }
                if it.row.start.is_some() {
                    c += 1;
                } else {
                    r += 1;
                }
            }
            it.row_start = r;
            it.col_start = c;
            if !dense {
                cur_r = r + rs;
                cur_c = c;
            }
        } else {
            let limit = cols.max(cs);
            let mut r = it.row.start.unwrap_or(if dense { 0 } else { cur_r });
            let mut c = it.col.start.unwrap_or(if dense || it.row.start.is_some() {
                0
            } else {
                cur_c
            });
            loop {
                if c + cs > limit && it.col.start.is_none() {
                    c = 0;
                    r += 1;
                    continue;
                }
                if free(&taken, r, c, rs, cs) {
                    break;
                }
                if it.col.start.is_some() {
                    r += 1;
                } else {
                    c += 1;
                }
            }
            it.row_start = r;
            it.col_start = c;
            if !dense {
                cur_r = r;
                cur_c = c + cs;
            }
        }
        mark(&mut taken, it.row_start, it.col_start, rs, cs);
        cols = cols.max(it.col_start + cs);
        rows = rows.max(it.row_start + rs);
    }
    (cols, rows)
}

#[derive(Clone, Copy)]
struct Track {
    base: f32,
    /// Growth limit; infinite for `auto`.
    limit: f32,
    flex: f32,
    /// `auto` / `fit-content`: grows to fill leftover space (stretch).
    auto: bool,
    /// The track's own size is content-based (min- or max-content).
    content: bool,
}

/// §12: Track sizing (intrinsic contributions, spans, maximise, `fr`, stretch).
#[allow(clippy::too_many_arguments)]
fn size_tracks(
    explicit: &[TrackDefinition],
    auto_tracks: &[TrackSizingFunction],
    track_count: usize,
    available: Option<f32>,
    gap: f32,
    items: &[GridItem],
    ctx: &LayoutContext,
    is_column: bool,
    column_sizes: &[f32],
    containing_width: f32,
    containing_height: f32,
) -> Vec<f32> {
    use TrackSizingFunction as T;
    let gaps = gap * track_count.saturating_sub(1) as f32;
    let sizing_for = |i: usize| -> T {
        if i < explicit.len() {
            explicit[i].sizing.clone()
        } else if !auto_tracks.is_empty() {
            auto_tracks[(i - explicit.len()) % auto_tracks.len()].clone()
        } else {
            T::Auto
        }
    };
    let avail_tracks = available.map(|a| (a - gaps).max(0.0));
    let mut tracks: Vec<Track> = Vec::with_capacity(track_count);
    for i in 0..track_count {
        let t = match sizing_for(i) {
            T::Length(px) => Track {
                base: px,
                limit: px,
                flex: 0.0,
                auto: false,
                content: false,
            },
            T::Percentage(p) => match avail_tracks {
                Some(a) => Track {
                    base: a * p,
                    limit: a * p,
                    flex: 0.0,
                    auto: false,
                    content: false,
                },
                None => Track {
                    base: 0.0,
                    limit: f32::INFINITY,
                    flex: 0.0,
                    auto: true,
                    content: true,
                },
            },
            T::Fr(f) => Track {
                base: 0.0,
                limit: 0.0,
                flex: f.max(0.0),
                auto: false,
                content: true,
            },
            T::Auto => Track {
                base: 0.0,
                limit: f32::INFINITY,
                flex: 0.0,
                auto: true,
                content: true,
            },
            T::MinContent | T::MaxContent | T::FitContent(_) => Track {
                base: 0.0,
                limit: f32::INFINITY,
                flex: 0.0,
                auto: false,
                content: true,
            },
            T::MinMax(min, max) => {
                let base = match *min {
                    T::Length(px) => px,
                    T::Percentage(p) => avail_tracks.map(|a| a * p).unwrap_or(0.0),
                    _ => 0.0,
                };
                match *max {
                    T::Length(px) => Track {
                        base: base.min(px),
                        limit: px,
                        flex: 0.0,
                        auto: false,
                        content: !matches!(*min, T::Length(_)),
                    },
                    T::Percentage(p) => {
                        let l = avail_tracks.map(|a| a * p).unwrap_or(f32::INFINITY);
                        Track {
                            base: base.min(l),
                            limit: l,
                            flex: 0.0,
                            auto: false,
                            content: !matches!(*min, T::Length(_)),
                        }
                    }
                    T::Fr(f) => Track {
                        base,
                        limit: base,
                        flex: f.max(0.0),
                        auto: false,
                        content: !matches!(*min, T::Length(_)),
                    },
                    _ => Track {
                        base,
                        limit: f32::INFINITY,
                        flex: 0.0,
                        auto: true,
                        content: !matches!(*min, T::Length(_)),
                    },
                }
            }
        };
        tracks.push(t);
    }

    // Item contributions (min-content, max-content) along this axis.
    let contribution = |it: &GridItem| -> (f32, f32) {
        let st = ctx.tree.style(it.node);
        let bm = BoxModel::resolve(st, containing_width);
        if is_column {
            let (mn, mx) = crate::intrinsic::intrinsic_widths(ctx, it.node);
            let m = bm.margin.horizontal();
            (mn + m, mx + m)
        } else {
            // Height at the item's column width.
            let w = span_size(
                column_sizes,
                it.col_start,
                (it.col_start + it.col_span).min(column_sizes.len()),
                gap_for_columns(ctx, it.node, column_sizes),
            );
            let w = if w > 0.0 { w } else { containing_width };
            let h = match st.height.resolve(containing_height) {
                Some(h) => h + bm.vertical_border_padding() + bm.margin.vertical(),
                None => {
                    let f = layout::layout_node(ctx, it.node, w, containing_height);
                    f.border_box().height + f.margin.vertical()
                }
            };
            (h, h)
        }
    };
    let span_of = |it: &GridItem| if is_column { it.col_span } else { it.row_span };
    let start_of = |it: &GridItem| {
        if is_column {
            it.col_start
        } else {
            it.row_start
        }
    };
    // Single-track spans first, then multi-track spans.
    for pass in [true, false] {
        for it in items {
            let span = span_of(it);
            if (span == 1) != pass {
                continue;
            }
            let from = start_of(it);
            let to = (from + span).min(tracks.len());
            if from >= to {
                continue;
            }
            let (mn, mx) = contribution(it);
            let content_tracks: Vec<usize> = (from..to).filter(|&i| tracks[i].content).collect();
            if content_tracks.is_empty() {
                continue;
            }
            let covered: f32 =
                (from..to).map(|i| tracks[i].base).sum::<f32>() + gap * (to - from - 1) as f32;
            let need_min = (mn - covered).max(0.0) / content_tracks.len() as f32;
            let covered_lim: f32 = (from..to)
                .map(|i| tracks[i].limit.min(1e9).max(tracks[i].base))
                .sum::<f32>()
                + gap * (to - from - 1) as f32;
            for &i in &content_tracks {
                if tracks[i].flex > 0.0 {
                    // `fr` tracks: min contribution only; max-content when no definite space.
                    tracks[i].base += need_min;
                    if avail_tracks.is_none() {
                        let extra = (mx - covered).max(0.0) / content_tracks.len() as f32;
                        tracks[i].base = tracks[i].base.max(tracks[i].base - need_min + extra);
                    }
                } else {
                    tracks[i].base += need_min;
                    if tracks[i].limit.is_infinite() {
                        let extra = (mx - covered_lim).max(0.0) / content_tracks.len() as f32;
                        let cur = if tracks[i].limit.is_finite() {
                            tracks[i].limit
                        } else {
                            0.0
                        };
                        tracks[i].limit = (cur.max(tracks[i].base) + extra).max(tracks[i].base);
                    }
                }
            }
        }
    }
    for t in &mut tracks {
        if t.limit.is_infinite() {
            // No item gave a max-content contribution.
            t.limit = t.base;
        }
        if t.limit < t.base {
            t.limit = t.base;
        }
        // `fit-content`/min-/max-content tracks keep their content size.
    }
    // Explicit `min-content` tracks stay at their minimum; `max-content` at their maximum.
    for (i, t) in tracks.iter_mut().enumerate() {
        match sizing_for(i) {
            T::MinContent => t.limit = t.base,
            T::MaxContent => t.base = t.limit,
            T::FitContent(l) => t.limit = t.limit.min(l.max(t.base)),
            _ => {}
        }
    }

    let mut sizes: Vec<f32> = tracks.iter().map(|t| t.base).collect();
    let Some(avail) = avail_tracks else {
        // Indefinite space (a container with auto height): fr tracks size to
        // their content; everything else is at its limit.
        for (i, t) in tracks.iter().enumerate() {
            if t.flex > 0.0 || t.auto || t.content {
                sizes[i] = t.limit.max(t.base);
            }
        }
        return sizes;
    };

    // Maximise: grow tracks towards their growth limits with the free space.
    let used: f32 = sizes.iter().sum();
    let mut free = avail - used;
    if free > 0.0 {
        let mut active: Vec<usize> = (0..tracks.len())
            .filter(|&i| tracks[i].flex == 0.0 && tracks[i].limit > sizes[i])
            .collect();
        while free > 0.001 && !active.is_empty() {
            let share = free / active.len() as f32;
            let mut next = Vec::new();
            for &i in &active {
                let room = tracks[i].limit - sizes[i];
                let add = share.min(room);
                sizes[i] += add;
                free -= add;
                if tracks[i].limit - sizes[i] > 0.001 {
                    next.push(i);
                }
            }
            if next.len() == active.len() && share < 0.001 {
                break;
            }
            active = next;
        }
    }

    // Flexible tracks share what is left, never going below their content minimum.
    let total_flex: f32 = tracks.iter().map(|t| t.flex).sum();
    if total_flex > 0.0 {
        let fixed: f32 = tracks
            .iter()
            .enumerate()
            .filter(|(_, t)| t.flex == 0.0)
            .map(|(i, _)| sizes[i])
            .sum();
        let mut leftover = (avail - fixed).max(0.0);
        let mut flex_left: Vec<usize> = (0..tracks.len())
            .filter(|&i| tracks[i].flex > 0.0)
            .collect();
        // Tracks whose minimum exceeds their share are frozen at the minimum.
        loop {
            let sum_f: f32 = flex_left
                .iter()
                .map(|&i| tracks[i].flex)
                .sum::<f32>()
                .max(1.0);
            let per = leftover / sum_f.max(1.0);
            let mut froze = false;
            flex_left.retain(|&i| {
                if per * tracks[i].flex < tracks[i].base {
                    sizes[i] = tracks[i].base;
                    leftover -= tracks[i].base;
                    froze = true;
                    false
                } else {
                    true
                }
            });
            if !froze {
                let sum_f: f32 = flex_left.iter().map(|&i| tracks[i].flex).sum::<f32>();
                let sum_f = if sum_f < 1.0 { 1.0 } else { sum_f };
                for &i in &flex_left {
                    sizes[i] = leftover.max(0.0) * tracks[i].flex / sum_f;
                }
                break;
            }
            if flex_left.is_empty() {
                break;
            }
        }
    } else {
        // Stretch auto tracks over any space still free.
        let free = avail - sizes.iter().sum::<f32>();
        let autos: Vec<usize> = (0..tracks.len()).filter(|&i| tracks[i].auto).collect();
        if free > 0.001 && !autos.is_empty() {
            let add = free / autos.len() as f32;
            for i in autos {
                sizes[i] += add;
            }
        }
    }
    sizes
}

/// The column gap, found from the grid container of `node`.
fn gap_for_columns(ctx: &LayoutContext, node: NodeId, _columns: &[f32]) -> f32 {
    ctx.tree
        .parent(node)
        .map(|p| ctx.tree.style(p).column_gap)
        .unwrap_or(0.0)
}

fn track_positions(sizes: &[f32], gap: f32) -> Vec<f32> {
    let mut positions = Vec::with_capacity(sizes.len());
    let mut pos = 0.0f32;
    for &size in sizes.iter() {
        positions.push(pos);
        pos += size + gap;
    }
    positions
}

fn effective_align_items(container: AlignItems, item: AlignSelf) -> AlignItems {
    match item {
        AlignSelf::Auto => container,
        AlignSelf::Stretch => AlignItems::Stretch,
        AlignSelf::FlexStart | AlignSelf::Start => AlignItems::Start,
        AlignSelf::FlexEnd | AlignSelf::End => AlignItems::End,
        AlignSelf::Center => AlignItems::Center,
        AlignSelf::Baseline => AlignItems::Baseline,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::{compute_layout, FixedWidthTextMeasure};
    use crate::style::ComputedStyle;
    use crate::tree::BoxTreeBuilder;

    #[test]
    fn test_grid_basic_2x2() {
        let mut builder = BoxTreeBuilder::new();
        let mut root_style = ComputedStyle {
            display: Display::GRID,
            ..ComputedStyle::block()
        };
        root_style.grid_template_columns = vec![
            TrackDefinition::new(TrackSizingFunction::Fr(1.0)),
            TrackDefinition::new(TrackSizingFunction::Fr(1.0)),
        ];
        root_style.grid_template_rows = vec![
            TrackDefinition::new(TrackSizingFunction::Length(50.0)),
            TrackDefinition::new(TrackSizingFunction::Length(50.0)),
        ];
        let root = builder.root(root_style);

        // 4 items for 2x2 grid
        for _ in 0..4 {
            let child_style = ComputedStyle::block();
            builder.element(root, child_style);
        }

        let tree = builder.build();
        let result = compute_layout(&tree, &FixedWidthTextMeasure, Size::new(800.0, 600.0));

        let children = tree.children(tree.root());
        let r0 = result.bounding_rect(children[0]).unwrap();
        let r1 = result.bounding_rect(children[1]).unwrap();
        let r2 = result.bounding_rect(children[2]).unwrap();
        let r3 = result.bounding_rect(children[3]).unwrap();

        // Each column should be 400px wide (800 / 2)
        assert!((r0.width - 400.0).abs() < 1.0);
        assert!((r1.width - 400.0).abs() < 1.0);

        // Positions
        assert!((r0.x - 0.0).abs() < 1.0);
        assert!((r1.x - 400.0).abs() < 1.0);
        assert!((r2.x - 0.0).abs() < 1.0);
        assert!((r3.x - 400.0).abs() < 1.0);

        assert!((r0.y - 0.0).abs() < 1.0);
        assert!((r1.y - 0.0).abs() < 1.0);
        assert!((r2.y - 50.0).abs() < 1.0);
        assert!((r3.y - 50.0).abs() < 1.0);
    }

    #[test]
    fn test_track_positions() {
        let sizes = vec![100.0, 200.0, 150.0];
        let positions = track_positions(&sizes, 10.0);
        assert_eq!(positions, vec![0.0, 110.0, 320.0]);
    }

    #[test]
    fn test_grid_with_gap() {
        let mut builder = BoxTreeBuilder::new();
        let mut root_style = ComputedStyle {
            display: Display::GRID,
            ..ComputedStyle::block()
        };
        root_style.grid_template_columns = vec![
            TrackDefinition::new(TrackSizingFunction::Fr(1.0)),
            TrackDefinition::new(TrackSizingFunction::Fr(1.0)),
        ];
        root_style.grid_template_rows =
            vec![TrackDefinition::new(TrackSizingFunction::Length(50.0))];
        root_style.column_gap = 20.0;
        let root = builder.root(root_style);

        builder.element(root, ComputedStyle::block());
        builder.element(root, ComputedStyle::block());

        let tree = builder.build();
        let result = compute_layout(&tree, &FixedWidthTextMeasure, Size::new(820.0, 600.0));

        let children = tree.children(tree.root());
        let r0 = result.bounding_rect(children[0]).unwrap();
        let r1 = result.bounding_rect(children[1]).unwrap();

        // (820 - 20 gap) / 2 = 400 each
        assert!((r0.width - 400.0).abs() < 1.0);
        assert!((r1.x - 420.0).abs() < 1.0); // 400 + 20 gap
    }
}
