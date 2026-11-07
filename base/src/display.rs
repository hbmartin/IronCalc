//! Retained calculation results and edit boundaries. None of this state is encoded.
use std::{
    collections::{HashMap, HashSet},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
};

use crate::{
    expressions::types::CellReferenceIndex,
    types::{ArrayKind, Cell, FormulaValue, Link},
    Model,
};

pub(crate) type Position = (u32, i32, i32);

#[derive(Default)]
pub(crate) struct DisplayState {
    pub(crate) edit_depth: usize,
    pub(crate) calculation_requested: bool,
    stored_reads: Arc<AtomicBool>,
    journal: Option<Arc<Mutex<MutationJournal>>>,
    #[cfg(test)]
    pub(crate) journal_positions: usize,
    #[cfg(test)]
    pub(crate) spill_owner_visits: usize,
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

/// One original image per touched position, shared by participating worksheets.
#[derive(Default, Debug)]
pub(crate) struct MutationJournal {
    cells: HashMap<Position, Option<CachedCell>>,
    links: HashMap<Position, Option<Link>>,
}

/// Cloning worksheet data never carries observation into the clone.
#[derive(Default, Debug)]
pub(crate) struct MutationObserver {
    sheet_id: u32,
    journal: Option<Arc<Mutex<MutationJournal>>>,
}
impl Clone for MutationObserver {
    fn clone(&self) -> Self {
        Self::default()
    }
}
impl MutationObserver {
    pub(crate) fn active(&self) -> bool {
        self.journal.is_some()
    }
    pub(crate) fn capture(&self, row: i32, column: i32, cell: Option<&Cell>) {
        if let Some(journal) = &self.journal {
            journal
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .cells
                .entry((self.sheet_id, row, column))
                .or_insert_with(|| cell.and_then(CachedCell::of));
        }
    }
}
impl CachedCell {
    fn of(cell: &Cell) -> Option<Self> {
        match cell {
            Cell::CellFormula { v, .. } => Some(Self::Formula(v.clone(), None)),
            Cell::ArrayFormula { v, r, kind, .. } => {
                Some(Self::Formula(v.clone(), Some((kind.clone(), *r))))
            }
            Cell::SpillCell { .. } => Some(Self::Spill(cell.clone())),
            _ => None,
        }
    }
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

    pub(crate) fn start_display_journal(&mut self) {
        debug_assert!(self.evaluation.display.journal.is_none());
        let journal = Arc::new(Mutex::new(MutationJournal::default()));
        for ws in &mut self.workbook.worksheets {
            ws.sheet_data.observe(MutationObserver {
                sheet_id: ws.sheet_id,
                journal: Some(Arc::clone(&journal)),
            });
        }
        self.evaluation.display.journal = Some(journal);
    }

    pub(crate) fn register_display_sheet(&mut self, sheet: u32, added: bool) {
        let Some(journal) = &self.evaluation.display.journal else {
            return;
        };
        let Some(ws) = self.workbook.worksheets.get_mut(sheet as usize) else {
            return;
        };
        if added {
            let mut records = journal.lock().unwrap_or_else(|e| e.into_inner());
            for (row, col, _) in ws.sheet_data.cells() {
                records.cells.entry((ws.sheet_id, row, col)).or_insert(None);
            }
        }
        ws.sheet_data.observe(MutationObserver {
            sheet_id: ws.sheet_id,
            journal: Some(Arc::clone(journal)),
        });
    }

    pub(crate) fn record_display_sheet_removal(&mut self, sheet: u32) {
        if self.evaluation.display.journal.is_some() {
            if let Some(ws) = self.workbook.worksheets.get_mut(sheet as usize) {
                ws.sheet_data.observe_removal();
            }
        }
    }

    pub(crate) fn take_display_journal(&mut self) -> DisplayDelta {
        let Some(journal) = self.evaluation.display.journal.take() else {
            return DisplayDelta::default();
        };
        for ws in &mut self.workbook.worksheets {
            ws.sheet_data.observe(MutationObserver::default());
        }
        let mut records = journal.lock().unwrap_or_else(|e| e.into_inner());
        #[cfg(test)]
        {
            self.evaluation.display.journal_positions = records.cells.len();
        }
        let sheets: HashMap<_, _> = self
            .workbook
            .worksheets
            .iter()
            .enumerate()
            .map(|(index, ws)| (ws.sheet_id, index))
            .collect();
        let cells = records
            .cells
            .drain()
            .filter_map(|(p, before)| {
                let after = sheets
                    .get(&p.0)
                    .and_then(|&i| self.workbook.worksheets[i].cell(p.1, p.2))
                    .as_deref()
                    .and_then(CachedCell::of);
                (before != after).then_some((p, before, after))
            })
            .collect();
        let links = records
            .links
            .drain()
            .filter_map(|(p, before)| {
                let after = sheets
                    .get(&p.0)
                    .and_then(|&i| self.links.get(&(i as u32, p.1, p.2)))
                    .cloned();
                (before != after).then_some((p, before, after))
            })
            .collect();
        DisplayDelta { cells, links }
    }

    fn record_dynamic_link(&self, position: (u32, i32, i32)) {
        if let Some(journal) = &self.evaluation.display.journal {
            if let Some(ws) = self.workbook.worksheets.get(position.0 as usize) {
                journal
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .links
                    .entry((ws.sheet_id, position.1, position.2))
                    .or_insert_with(|| self.links.get(&position).cloned());
            }
        }
    }
    pub(crate) fn insert_dynamic_link(&mut self, position: (u32, i32, i32), link: Link) {
        self.record_dynamic_link(position);
        self.links.insert(position, link);
    }
    pub(crate) fn remove_dynamic_link(&mut self, position: &(u32, i32, i32)) {
        if self.links.contains_key(position) {
            self.record_dynamic_link(*position);
            self.links.remove(position);
        }
    }
    pub(crate) fn clear_dynamic_links(&mut self) {
        if self.evaluation.display.journal.is_some() {
            for &position in self.links.keys() {
                self.record_dynamic_link(position);
            }
        }
        self.links.clear();
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
        let positions: Vec<_> = if let Some(journal) = &self.evaluation.display.journal {
            let records = journal.lock().unwrap_or_else(|e| e.into_inner());
            records
                .cells
                .keys()
                .filter_map(|p| {
                    self.workbook
                        .worksheets
                        .iter()
                        .position(|ws| ws.sheet_id == p.0)
                        .map(|i| (i as u32, p.1, p.2))
                })
                .collect()
        } else {
            self.links.keys().copied().collect()
        };
        for position in positions {
            if !self
                .workbook
                .worksheets
                .get(position.0 as usize)
                .and_then(|ws| ws.cell(position.1, position.2))
                .is_some_and(|c| c.has_formula())
            {
                self.remove_dynamic_link(&position);
            }
        }
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
                    self.insert_dynamic_link(position, link.clone());
                }
            } else {
                self.remove_dynamic_link(&position);
            }
        }
    }

    fn record_snapshot_links(&self, before: &DisplaySnapshot) {
        if let Some(journal) = &self.evaluation.display.journal {
            let mut records = journal.lock().unwrap_or_else(|e| e.into_inner());
            for (&p, link) in &before.links {
                records.links.entry(p).or_insert_with(|| Some(link.clone()));
            }
        }
    }

    /// Preserve derived fields while structural code rewrites formulas and cells.
    pub(crate) fn with_structural_display_edit<T>(
        &mut self,
        map: impl Fn(Position) -> Option<Position>,
        edit: impl FnOnce(&mut Self) -> Result<T, String>,
    ) -> Result<T, String> {
        let before = self.display_snapshot();
        self.record_snapshot_links(&before);
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
        // These identities come from before the structural edit, not shifted indices.
        self.record_snapshot_links(&before);
        if let Some(journal) = &self.evaluation.display.journal {
            let mut records = journal.lock().unwrap_or_else(|e| e.into_inner());
            for &position in before.links.keys() {
                if let Some(target) = map(position) {
                    records.links.entry(target).or_insert(None);
                }
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
        let mut owners = HashSet::new();
        let inside = |r, c| {
            r >= area.row
                && r < area.row + area.height
                && c >= area.column
                && c < area.column + area.width
        };
        #[cfg(test)]
        let mut visits = 0;
        for row in area.row..area.row + area.height {
            for (_, cell) in
                ws.sheet_data
                    .cells_in_row_between(row, area.column, area.column + area.width - 1)
            {
                #[cfg(test)]
                {
                    visits += 1;
                }
                if let Cell::SpillCell { a, .. } = &*cell {
                    if !inside(a.0, a.1) {
                        owners.insert(*a);
                    }
                }
            }
        }
        let anchors: Vec<_> = owners
            .into_iter()
            .filter_map(|(r, c)| match ws.cell(r, c).as_deref() {
                Some(Cell::ArrayFormula {
                    kind: ArrayKind::Dynamic,
                    r: (w, h),
                    ..
                }) => Some((r, c, *w, *h)),
                _ => None,
            })
            .collect();
        #[cfg(test)]
        {
            self.evaluation.display.spill_owner_visits += visits;
        }
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
    #[cfg(test)]
    pub(crate) fn cell_count(&self) -> usize {
        self.cells.len()
    }

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
