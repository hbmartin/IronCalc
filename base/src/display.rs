//! Retained calculation results and edit boundaries. None of this state is encoded.
use std::{
    collections::{HashMap, HashSet},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
};

use crate::{
    expressions::types::CellReferenceIndex,
    types::{ArrayKind, Cell, FormulaValue, Link},
    Model,
};

type Position = (u32, i32, i32);

#[derive(Default)]
pub(crate) struct DisplayState {
    pub(crate) edit_depth: usize,
    pub(crate) calculation_requested: bool,
    stored_reads: Arc<AtomicBool>,
}

pub(crate) struct StoredReadsGuard(Arc<AtomicBool>, bool);
impl Drop for StoredReadsGuard {
    fn drop(&mut self) {
        self.0.store(self.1, Ordering::Relaxed);
    }
}

#[derive(Clone, PartialEq, Debug)]
pub(crate) enum CachedCell {
    Formula(FormulaValue, Option<(ArrayKind, (i32, i32))>),
    Spill(Cell),
}

#[derive(Default)]
pub(crate) struct DisplaySnapshot {
    // Sheet ids, rather than vector indices, survive sheet reordering and deletion.
    cells: HashMap<Position, CachedCell>,
    links: HashMap<Position, Link>,
}

#[derive(Clone, Default)]
pub(crate) struct DisplayDelta {
    cells: Vec<(Position, Option<CachedCell>, Option<CachedCell>)>,
    links: Vec<(Position, Option<Link>, Option<Link>)>,
}

fn delta<T: Clone + PartialEq>(
    before: HashMap<Position, T>,
    after: HashMap<Position, T>,
) -> Vec<(Position, Option<T>, Option<T>)> {
    let mut after = after;
    let mut result = Vec::new();
    for (key, old) in before {
        let new = after.remove(&key);
        if new.as_ref() != Some(&old) {
            result.push((key, Some(old), new));
        }
    }
    result.extend(after.into_iter().map(|(key, new)| (key, None, Some(new))));
    result
}

impl DisplaySnapshot {
    pub(crate) fn difference(self, after: Self) -> DisplayDelta {
        DisplayDelta {
            cells: delta(self.cells, after.cells),
            links: delta(self.links, after.links),
        }
    }
}

impl Model<'_> {
    pub(crate) fn reading_stored_values(&self) -> bool {
        self.evaluation.display.stored_reads.load(Ordering::Relaxed)
    }

    #[must_use]
    pub(crate) fn start_stored_reads(&self) -> StoredReadsGuard {
        let flag = Arc::clone(&self.evaluation.display.stored_reads);
        let previous = flag.swap(true, Ordering::Relaxed);
        StoredReadsGuard(flag, previous)
    }

    pub(crate) fn begin_display_edit(&mut self) {
        self.evaluation.display.edit_depth += 1;
    }

    pub(crate) fn cancel_display_edit(&mut self) {
        self.evaluation.display.edit_depth -= 1;
        if self.evaluation.display.edit_depth == 0 {
            self.evaluation.display.calculation_requested = false;
        }
    }

    pub(crate) fn finish_display_edit(&mut self, calculate: bool) {
        self.evaluation.display.edit_depth -= 1;
        if self.evaluation.display.edit_depth != 0 {
            return;
        }
        self.links.retain(|&(sheet, row, column), _| {
            self.workbook
                .worksheets
                .get(sheet as usize)
                .and_then(|ws| ws.cell(row, column))
                .is_some_and(|cell| cell.has_formula())
        });
        let requested = std::mem::take(&mut self.evaluation.display.calculation_requested);
        if calculate && requested {
            self.evaluate();
        } else {
            self.evaluate_conditional_formatting();
        }
    }

    pub(crate) fn display_snapshot(&self) -> DisplaySnapshot {
        let mut result = DisplaySnapshot::default();
        for ws in &self.workbook.worksheets {
            for (row, column, cell) in ws.sheet_data.cells() {
                let value = match &*cell {
                    Cell::CellFormula { v, .. } => CachedCell::Formula(v.clone(), None),
                    Cell::ArrayFormula { v, r, kind, .. } => {
                        CachedCell::Formula(v.clone(), Some((kind.clone(), *r)))
                    }
                    Cell::SpillCell { .. } => CachedCell::Spill(cell.into_owned()),
                    _ => continue,
                };
                result.cells.insert((ws.sheet_id, row, column), value);
            }
        }
        for (&(sheet, row, column), link) in &self.links {
            if let Some(ws) = self.workbook.worksheets.get(sheet as usize) {
                result
                    .links
                    .insert((ws.sheet_id, row, column), link.clone());
            }
        }
        result
    }

    fn restore_cached_cell(&mut self, sheet: u32, row: i32, column: i32, cached: &CachedCell) {
        let Some(ws) = self.workbook.worksheets.get_mut(sheet as usize) else {
            return;
        };
        match cached {
            CachedCell::Formula(value, array) => {
                if let Some(cell) = ws.sheet_data.cell_mut(row, column) {
                    let (f, s) = match cell {
                        Cell::CellFormula { f, s, .. } | Cell::ArrayFormula { f, s, .. } => {
                            (*f, *s)
                        }
                        _ => return,
                    };
                    *cell = match array {
                        Some((kind, r)) => Cell::ArrayFormula {
                            f,
                            s,
                            v: value.clone(),
                            kind: kind.clone(),
                            r: *r,
                        },
                        None => Cell::CellFormula {
                            f,
                            s,
                            v: value.clone(),
                        },
                    };
                }
            }
            CachedCell::Spill(cell) => {
                // Never overwrite user data with a retained spill.
                if matches!(
                    ws.cell(row, column).as_deref(),
                    None | Some(Cell::EmptyCell { .. } | Cell::SpillCell { .. })
                ) {
                    let _ = ws.update_cell(row, column, cell.clone());
                }
            }
        }
    }

    pub(crate) fn apply_display_delta(&mut self, delta: &DisplayDelta, undo: bool) {
        for (key, before, after) in &delta.cells {
            let Some(sheet) = self
                .workbook
                .worksheets
                .iter()
                .position(|ws| ws.sheet_id == key.0)
            else {
                continue;
            };
            match if undo { before } else { after } {
                Some(value) => self.restore_cached_cell(sheet as u32, key.1, key.2, value),
                None => {
                    let ws = &mut self.workbook.worksheets[sheet];
                    if matches!(
                        ws.cell(key.1, key.2).as_deref(),
                        Some(Cell::SpillCell { .. })
                    ) {
                        let _ = ws.cell_clear_contents(key.1, key.2);
                    }
                }
            }
        }
        for (key, before, after) in &delta.links {
            let Some(sheet) = self
                .workbook
                .worksheets
                .iter()
                .position(|ws| ws.sheet_id == key.0)
            else {
                continue;
            };
            let position = (sheet as u32, key.1, key.2);
            if let Some(link) = if undo { before } else { after } {
                if self.workbook.worksheets[sheet]
                    .cell(key.1, key.2)
                    .is_some_and(|c| c.has_formula())
                {
                    self.links.insert(position, link.clone());
                }
            } else {
                self.links.remove(&position);
            }
        }
    }

    /// Preserve derived fields while the existing structural implementation
    /// rewrites formulas and cells. `map` describes the logical cell movement.
    pub(crate) fn with_structural_display_edit<T>(
        &mut self,
        map: impl Fn(Position) -> Option<Position>,
        edit: impl FnOnce(&mut Self) -> Result<T, String>,
    ) -> Result<T, String> {
        let before = self.display_snapshot();
        self.begin_display_edit();
        let result = edit(self);
        if result.is_ok() {
            self.restore_mapped_display(before, map);
        }
        self.finish_display_edit(result.is_ok());
        result
    }

    pub(crate) fn restore_mapped_display(
        &mut self,
        before: DisplaySnapshot,
        map: impl Fn(Position) -> Option<Position>,
    ) {
        let sheet_index = |model: &Self, id| {
            model
                .workbook
                .worksheets
                .iter()
                .position(|ws| ws.sheet_id == id)
                .map(|s| s as u32)
        };
        // Whether an entire array still forms the same rectangle at one offset.
        let mut groups = HashMap::new();
        for (&position, cached) in &before.cells {
            if let CachedCell::Formula(_, Some((_, (width, height)))) = cached {
                if let Some(target) = map(position) {
                    let offset = (target.0, target.1 - position.1, target.2 - position.2);
                    let intact = (0..*height).all(|r| {
                        (0..*width).all(|c| {
                            map((position.0, position.1 + r, position.2 + c))
                                == Some((
                                    offset.0,
                                    position.1 + r + offset.1,
                                    position.2 + c + offset.2,
                                ))
                        })
                    });
                    groups.insert(position, (target, intact));
                }
            }
        }
        // A cut may leave old spill children in place. Remove every child of a
        // broken group, including surviving fragments, before restoring intact groups.
        for (&position, cached) in &before.cells {
            if let CachedCell::Spill(Cell::SpillCell { a, .. }) = cached {
                let anchor = (position.0, a.0, a.1);
                if groups.get(&anchor).is_some_and(|g| g.1) {
                    continue;
                }
                for candidate in [Some(position), map(position)].into_iter().flatten() {
                    if let Some(sheet) = sheet_index(self, candidate.0) {
                        let ws = &mut self.workbook.worksheets[sheet as usize];
                        let new_anchor = groups.get(&anchor).map(|g| (g.0 .1, g.0 .2));
                        if matches!(ws.cell(candidate.1,candidate.2).as_deref(), Some(Cell::SpillCell { a: owner, .. }) if *owner == *a || Some(*owner) == new_anchor)
                        {
                            let _ = ws.cell_clear_contents(candidate.1, candidate.2);
                        }
                    }
                }
            }
        }
        for (&position, cached) in &before.cells {
            let Some(target) = map(position) else {
                continue;
            };
            let Some(sheet) = sheet_index(self, target.0) else {
                continue;
            };
            match cached {
                CachedCell::Formula(v, array) => {
                    let array = array.clone().map(|(kind, range)| {
                        (
                            kind,
                            if groups.get(&position).is_some_and(|g| g.1) {
                                range
                            } else {
                                (1, 1)
                            },
                        )
                    });
                    self.restore_cached_cell(
                        sheet,
                        target.1,
                        target.2,
                        &CachedCell::Formula(v.clone(), array),
                    );
                }
                CachedCell::Spill(Cell::SpillCell { a, .. }) => {
                    let anchor = (position.0, a.0, a.1);
                    if let Some(&(new_anchor, true)) = groups.get(&anchor) {
                        if let CachedCell::Spill(mut cell) = cached.clone() {
                            if let Cell::SpillCell { a, .. } = &mut cell {
                                *a = (new_anchor.1, new_anchor.2);
                            }
                            self.restore_cached_cell(
                                sheet,
                                target.1,
                                target.2,
                                &CachedCell::Spill(cell),
                            );
                        }
                    }
                }
                _ => {}
            }
        }
        // Replace the runtime map, removing deleted cells and overwritten targets.
        self.links.clear();
        for (position, link) in before.links {
            if let Some(target) = map(position) {
                if let Some(sheet) = sheet_index(self, target.0) {
                    if self.workbook.worksheets[sheet as usize]
                        .cell(target.1, target.2)
                        .is_some_and(|c| c.has_formula())
                    {
                        self.links.insert((sheet, target.1, target.2), link);
                    }
                }
            }
        }
    }

    pub(crate) fn display_identity(position: Position) -> Option<Position> {
        Some(position)
    }

    pub(crate) fn stored_display_value(
        &self,
        cell: CellReferenceIndex,
    ) -> crate::calc_result::CalcResult {
        self.stored_value(cell)
    }
}

pub(crate) enum AxisEdit {
    Insert(i32),
    Delete(i32),
    Move { count: i32, delta: i32 },
}

pub(crate) fn map_axis(
    p: Position,
    sheet: u32,
    rows: bool,
    start: i32,
    edit: AxisEdit,
) -> Option<Position> {
    if p.0 != sheet {
        return Some(p);
    }
    let old = if rows { p.1 } else { p.2 };
    let new = match edit {
        AxisEdit::Insert(count) => {
            if old >= start {
                old + count
            } else {
                old
            }
        }
        AxisEdit::Delete(count) => {
            if old < start {
                old
            } else if old < start + count {
                return None;
            } else {
                old - count
            }
        }
        AxisEdit::Move { count, delta } => {
            if count <= 0 || delta == 0 {
                return Some(p);
            }
            if old >= start && old < start + count {
                old + delta
            } else if delta > 0 && old >= start + count && old < start + count + delta {
                old - count
            } else if delta < 0 && old >= start + delta && old < start {
                old + count
            } else {
                old
            }
        }
    };
    Some(if rows {
        (sheet, new, p.2)
    } else {
        (sheet, p.1, new)
    })
}

impl Model<'_> {
    pub(crate) fn with_display_batch<T>(
        &mut self,
        edit: impl FnOnce(&mut Self) -> Result<T, String>,
    ) -> Result<T, String> {
        self.begin_display_edit();
        let result = edit(self);
        self.finish_display_edit(result.is_ok());
        result
    }
}

impl Model<'_> {
    pub(crate) fn validate_context_calculation(&self) -> Result<(), String> {
        let settings = &self.workbook.settings.iterative_calculation;
        if settings.enabled && settings.validate().is_err() {
            return Err(
                "Cannot change calculation context with invalid iterative calculation settings"
                    .into(),
            );
        }
        Ok(())
    }
}

impl Model<'_> {
    pub(crate) fn collapse_partially_cleared_spills(
        &mut self,
        area: &crate::expressions::types::Area,
    ) -> Result<(), String> {
        let ws = self.workbook.worksheet(area.sheet)?;
        let anchors: Vec<_> = ws
            .sheet_data
            .cells()
            .filter_map(|(row, column, cell)| {
                if let Cell::ArrayFormula {
                    kind: ArrayKind::Dynamic,
                    r: (width, height),
                    ..
                } = &*cell
                {
                    let inside = row >= area.row
                        && row < area.row + area.height
                        && column >= area.column
                        && column < area.column + area.width;
                    let overlaps = row < area.row + area.height
                        && row + height > area.row
                        && column < area.column + area.width
                        && column + width > area.column;
                    if overlaps && !inside {
                        return Some((row, column, *width, *height));
                    }
                }
                None
            })
            .collect();
        let ws = self.workbook.worksheet_mut(area.sheet)?;
        for (row, column, width, height) in anchors {
            for r in row..row + height {
                for c in column..column + width {
                    if matches!(ws.cell(r,c).as_deref(),Some(Cell::SpillCell {a,..}) if *a==(row,column))
                    {
                        let _ = ws.cell_clear_contents(r, c);
                    }
                }
            }
            if let Some(Cell::ArrayFormula { r, .. }) = ws.sheet_data.cell_mut(row, column) {
                *r = (1, 1);
            }
        }
        Ok(())
    }
}

/// Explicit remote writes retire local derived state without changing wire history.
#[derive(Default)]
pub(crate) struct DisplayConflicts {
    pub(crate) all: bool,
    pub(crate) sheets: HashSet<u32>,
    pub(crate) areas: Vec<(u32, i32, i32, i32, i32)>,
}
impl DisplayConflicts {
    pub(crate) fn area(&mut self, model: &Model, area: &crate::expressions::types::Area) {
        let Ok(ws) = model.workbook.worksheet(area.sheet) else {
            return;
        };
        self.areas
            .push((ws.sheet_id, area.row, area.column, area.height, area.width));
        for row in area.row..area.row.saturating_add(area.height) {
            for (column, cell) in ws.sheet_data.cells_in_row_between(
                row,
                area.column,
                area.column.saturating_add(area.width - 1),
            ) {
                let (ar, ac) = match &*cell {
                    Cell::SpillCell { a, .. } => *a,
                    Cell::ArrayFormula { .. } => (row, column),
                    _ => continue,
                };
                if let Some(Cell::ArrayFormula { r: (w, h), .. }) = ws.cell(ar, ac).as_deref() {
                    self.areas.push((ws.sheet_id, ar, ac, *h, *w));
                }
            }
        }
    }
    fn contains(&self, p: Position) -> bool {
        self.all
            || self.sheets.contains(&p.0)
            || self.areas.iter().any(|&(s, r, c, h, w)| {
                p.0 == s
                    && p.1 >= r
                    && p.1 < r.saturating_add(h)
                    && p.2 >= c
                    && p.2 < c.saturating_add(w)
            })
    }
}
impl DisplayDelta {
    pub(crate) fn discard_conflicts(&mut self, conflicts: &DisplayConflicts) {
        let mut owners = HashSet::new();
        for (p, before, after) in &self.cells {
            for value in [before, after].into_iter().flatten() {
                match value {
                    CachedCell::Spill(Cell::SpillCell { a, .. }) if conflicts.contains(*p) => {
                        owners.insert((p.0, a.0, a.1));
                    }
                    CachedCell::Formula(_, Some((_, (w, h))))
                        if conflicts.areas.iter().any(|&(s, r, c, ah, aw)| {
                            s == p.0
                                && p.1 < r.saturating_add(ah)
                                && p.1.saturating_add(*h) > r
                                && p.2 < c.saturating_add(aw)
                                && p.2.saturating_add(*w) > c
                        }) =>
                    {
                        owners.insert(*p);
                    }
                    _ => {}
                }
            }
        }
        self.cells.retain(|(position, before, after)| {
            if conflicts.contains(*position) || owners.contains(position) {
                return false;
            }
            ![before, after].into_iter().flatten().any(|value| {
                if let CachedCell::Spill(Cell::SpillCell { a, .. }) = value {
                    let owner = (position.0, a.0, a.1);
                    owners.contains(&owner) || conflicts.contains(owner)
                } else {
                    false
                }
            })
        });
        self.links
            .retain(|(p, _, _)| !conflicts.contains(*p) && !owners.contains(p));
    }
}
