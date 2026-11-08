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

pub(crate) trait DisplayEditTarget<'a> {
    fn edit_model(&mut self) -> &mut Model<'a>;
    fn calculate_after_edit(&self) -> bool {
        true
    }
}
impl<'a> DisplayEditTarget<'a> for Model<'a> {
    fn edit_model(&mut self) -> &mut Model<'a> {
        self
    }
}

/// Owns the mutable borrow through final calculation and journal publication.
/// Dropping an unfinished scope cleans up bookkeeping, never workbook values.
#[must_use]
pub(crate) struct DisplayEditGuard<'s, 'a, T: DisplayEditTarget<'a>> {
    target: &'s mut T,
    previous_depth: usize,
    armed: bool,
    lifetime: std::marker::PhantomData<&'a ()>,
}
impl<'s, 'a, T: DisplayEditTarget<'a>> DisplayEditGuard<'s, 'a, T> {
    pub(crate) fn new(target: &'s mut T) -> Self {
        let model = target.edit_model();
        let previous_depth = model.evaluation.display.edit_depth;
        model.begin_display_edit();
        Self {
            target,
            previous_depth,
            armed: true,
            lifetime: std::marker::PhantomData,
        }
    }
    pub(crate) fn run<R>(
        target: &'s mut T,
        edit: impl FnOnce(&mut T) -> Result<R, String>,
        prepare: impl FnOnce(&mut T),
        publish: impl FnOnce(&mut T),
    ) -> Result<R, String> {
        let mut scope = Self::new(target);
        let result = edit(scope.target());
        if result.is_ok() {
            prepare(scope.target());
            let calculate = scope.target().calculate_after_edit();
            scope.finish(calculate);
        } else {
            // Live edits may have changed cells before returning an error.
            // Reconcile their display without running formula calculation.
            scope
                .target()
                .edit_model()
                .finish_display_edit(false, false);
        }
        publish(scope.target());
        scope.complete();
        result
    }
    pub(crate) fn target(&mut self) -> &mut T {
        self.target
    }
    pub(crate) fn finish(&mut self, calculate: bool) {
        self.target
            .edit_model()
            .finish_display_edit(calculate, true);
    }
    pub(crate) fn cancel(&mut self) {
        self.target.edit_model().cancel_display_edit();
    }
    pub(crate) fn complete(mut self) {
        self.armed = false;
    }
}
impl<'a, T: DisplayEditTarget<'a>> Drop for DisplayEditGuard<'_, 'a, T> {
    fn drop(&mut self) {
        if self.armed {
            self.target
                .edit_model()
                .abort_display_edit(self.previous_depth);
        }
    }
}

#[derive(Default)]
pub(crate) struct DisplayState {
    pub(crate) edit_depth: usize,
    pub(crate) calculation_requested: bool,
    pub(crate) formatting_requested: bool,
    pub(crate) formatting_recovery: bool,
    stored_reads: Arc<AtomicBool>,
    journal: Option<Arc<Mutex<MutationJournal>>>,
    #[cfg(test)]
    pub(crate) journal_positions: usize,
    #[cfg(test)]
    pub(crate) spill_owner_visits: usize,
    #[cfg(test)]
    pub(crate) conflict_batches: usize,
    #[cfg(test)]
    pub(crate) history_scans: usize,
    #[cfg(test)]
    pub(crate) paste_cells: usize,
    #[cfg(test)]
    pub(crate) cse_discovery_visits: usize,
    #[cfg(test)]
    pub(crate) paste_hook: Option<fn(&mut Model<'_>)>,
    #[cfg(test)]
    pub(crate) cf_hook: Option<fn()>,
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
    replay_cells: HashSet<Position>,
    replay_links: HashSet<Position>,
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
            let mut records = journal.lock().unwrap_or_else(|e| e.into_inner());
            let position = (self.sheet_id, row, column);
            records
                .cells
                .entry(position)
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
        let replay_cells = std::mem::take(&mut records.replay_cells);
        let replay_links = std::mem::take(&mut records.replay_links);
        let cells = records
            .cells
            .drain()
            .filter_map(|(p, before)| {
                let after = sheets
                    .get(&p.0)
                    .and_then(|&i| self.workbook.worksheets[i].cell(p.1, p.2))
                    .as_deref()
                    .and_then(CachedCell::of);
                (before != after
                    || (replay_cells.contains(&p) && (before.is_some() || after.is_some())))
                .then_some((p, before, after))
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
                (before != after
                    || (replay_links.contains(&p) && (before.is_some() || after.is_some())))
                .then_some((p, before, after))
            })
            .collect();
        DisplayDelta { cells, links }
    }

    pub(crate) fn prepared_display_cells(&self) -> Vec<(u32, i32, i32, Option<Cell>)> {
        let Some(journal) = &self.evaluation.display.journal else {
            return Vec::new();
        };
        journal
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .cells
            .keys()
            .filter_map(|&(id, r, c)| {
                let sheet = self.get_sheet_index_by_sheet_id(id)?;
                Some((
                    sheet,
                    r,
                    c,
                    self.workbook.worksheets[sheet as usize]
                        .cell(r, c)
                        .map(std::borrow::Cow::into_owned),
                ))
            })
            .collect()
    }
    pub(crate) fn prepared_dynamic_links(&self) -> Vec<((u32, i32, i32), Option<Link>)> {
        let Some(journal) = &self.evaluation.display.journal else {
            return Vec::new();
        };
        journal
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .links
            .keys()
            .filter_map(|&(id, r, c)| {
                let sheet = self.get_sheet_index_by_sheet_id(id)?;
                let p = (sheet, r, c);
                Some((p, self.links.get(&p).cloned()))
            })
            .collect()
    }
    pub(crate) fn retain_prepared_replay(&self, delta: &DisplayDelta) {
        if let Some(journal) = &self.evaluation.display.journal {
            let mut records = journal.lock().unwrap_or_else(|e| e.into_inner());
            records
                .replay_cells
                .extend(delta.cells.iter().map(|(p, _, _)| *p));
            records
                .replay_links
                .extend(delta.links.iter().map(|(p, _, _)| *p));
        }
    }

    /// Re-entering a rewritten formula also destroys equal caches on redo.
    pub(crate) fn retain_formula_replay(&self, area: &crate::expressions::types::Area) {
        if let Some(journal) = &self.evaluation.display.journal {
            let id = self.workbook.worksheets[area.sheet as usize].sheet_id;
            let mut records = journal.lock().unwrap_or_else(|e| e.into_inner());
            for row in area.row..area.row + area.height {
                for column in area.column..area.column + area.width {
                    let p = (id, row, column);
                    records.replay_cells.insert(p);
                    records.replay_links.insert(p);
                }
            }
        }
    }

    fn record_dynamic_link(&self, position: (u32, i32, i32)) {
        if let Some(journal) = &self.evaluation.display.journal {
            if let Some(ws) = self.workbook.worksheets.get(position.0 as usize) {
                let mut records = journal.lock().unwrap_or_else(|e| e.into_inner());
                let key = (ws.sheet_id, position.1, position.2);
                records
                    .links
                    .entry(key)
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
            self.evaluation.display.formatting_requested = false;
        }
    }

    pub(crate) fn finish_display_edit(&mut self, calculate: bool, recover: bool) {
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
        let formatting = std::mem::take(&mut self.evaluation.display.formatting_requested);
        if calculate && requested {
            self.evaluate();
        } else if formatting || (recover && self.evaluation.display.formatting_recovery) {
            self.evaluate_conditional_formatting();
        }
    }

    pub(crate) fn abort_display_edit(&mut self, previous_depth: usize) {
        self.evaluation.display.edit_depth = previous_depth;
        self.evaluation.display.journal = None;
        for ws in &mut self.workbook.worksheets {
            ws.sheet_data.observe(MutationObserver::default());
        }
        self.invalidate_calculation();
        self.evaluation.display.calculation_requested = false;
        self.evaluation.display.formatting_requested = false;
        self.evaluation.display.formatting_recovery = true;
        self.evaluation.stack.clear();
        self.evaluation.seen.clear();
        self.evaluation.seen_ranges.clear();
        self.evaluation.root = None;
        self.evaluation.restart = None;
        self.evaluation.in_pass = false;
        self.evaluation.iterating = false;
        self.evaluation.unwinding = false;
        self.evaluation.depth = 0;
        self.evaluation.lambda_depth = 0;
    }

    /// Transfer derived fields after re-entering a moved formula at its target.
    pub(crate) fn transfer_cell_display(
        &mut self,
        sheet: u32,
        source: (i32, i32),
        target: (i32, i32),
        cell: &Cell,
    ) {
        if let Some(mut cached) = CachedCell::of(cell) {
            if let CachedCell::Formula(_, Some((ArrayKind::Dynamic, extent))) = &mut cached {
                *extent = (1, 1);
            }
            self.restore_cached_cell(sheet, target.0, target.1, &cached);
        }
        let old = (sheet, source.0, source.1);
        let new = (sheet, target.0, target.1);
        let link = self.links.get(&old).cloned();
        self.remove_dynamic_link(&old);
        self.remove_dynamic_link(&new);
        if let Some(link) = link {
            self.insert_dynamic_link(new, link);
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
        if ws
            .cell(row, column)
            .as_deref()
            .and_then(CachedCell::of)
            .as_ref()
            == Some(cached)
        {
            return;
        }
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
                if ws.is_covered_cell(row, column) {
                    return;
                }
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

    pub(crate) fn record_snapshot_links(&self, before: &DisplaySnapshot) {
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
        DisplayEditGuard::run(
            self,
            edit,
            |model| {
                model.restore_mapped_display(before, map);
                model.evaluation.display.formatting_requested = true;
            },
            |_| {},
        )
    }

    pub(crate) fn restore_mapped_display(
        &mut self,
        before: DisplaySnapshot,
        map: impl Fn(Position) -> Option<Position>,
    ) {
        // Cuts remap outside a structural scope: retain their replay positions too.
        if let Some(journal) = &self.evaluation.display.journal {
            let mut records = journal.lock().unwrap_or_else(|e| e.into_inner());
            for &p in before.cells.keys() {
                if map(p) != Some(p) {
                    records.replay_cells.insert(p);
                    if let Some(target) = map(p) {
                        records.replay_cells.insert(target);
                    }
                }
            }
            for &p in before.links.keys() {
                if map(p) != Some(p) {
                    records.replay_links.insert(p);
                    if let Some(target) = map(p) {
                        records.replay_links.insert(target);
                    }
                }
            }
        }
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
        DisplayEditGuard::run(self, edit, |_| {}, |_| {})
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
    pub(crate) areas: HashSet<(u32, i32, i32, i32, i32)>,
}
impl DisplayConflicts {
    pub(crate) fn area(&mut self, model: &Model, area: &crate::expressions::types::Area) {
        let Ok(ws) = model.workbook.worksheet(area.sheet) else {
            return;
        };
        self.areas
            .insert((ws.sheet_id, area.row, area.column, area.height, area.width));
        let mut owners = HashSet::new();
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
                if owners.insert((ar, ac)) {
                    if let Some(Cell::ArrayFormula { r: (w, h), .. }) = ws.cell(ar, ac).as_deref() {
                        self.areas.insert((ws.sheet_id, ar, ac, *h, *w));
                    }
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
    pub(crate) fn is_empty(&self) -> bool {
        !self.all && self.sheets.is_empty() && self.areas.is_empty()
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
    pub(crate) fn intersects(&self, conflicts: &DisplayConflicts) -> bool {
        self.cells.iter().any(|(p, before, after)| {
            conflicts.contains(*p)
                || [before, after].into_iter().flatten().any(|v| match v {
                    CachedCell::Formula(_, Some((_, (w, h)))) => {
                        conflicts.areas.iter().any(|&(s, r, c, ah, aw)| {
                            s == p.0
                                && p.1 < r.saturating_add(ah)
                                && p.1.saturating_add(*h) > r
                                && p.2 < c.saturating_add(aw)
                                && p.2.saturating_add(*w) > c
                        })
                    }
                    CachedCell::Spill(Cell::SpillCell { a, .. }) => {
                        conflicts.contains((p.0, a.0, a.1))
                    }
                    _ => false,
                })
        }) || self.links.iter().any(|(p, _, _)| conflicts.contains(*p))
    }
}

#[cfg(test)]
mod rectangle_tests {
    use super::*;

    #[test]
    fn extreme_extents_intersect_and_prune_consistently() {
        let mut delta = DisplayDelta {
            cells: vec![(
                (0, 3, 3),
                None,
                Some(CachedCell::Formula(
                    FormulaValue::Number(1.0),
                    Some((ArrayKind::Dynamic, (i32::MAX, i32::MAX))),
                )),
            )],
            links: Vec::new(),
        };
        let mut conflicts = DisplayConflicts::default();
        conflicts.areas.insert((0, 4, 4, i32::MAX, i32::MAX));
        assert!(delta.intersects(&conflicts));
        let mut other_sheet = DisplayConflicts::default();
        other_sheet.areas.insert((1, 4, 4, i32::MAX, i32::MAX));
        assert!(!delta.intersects(&other_sheet));
        delta.discard_conflicts(&conflicts);
        assert_eq!(delta.cell_count(), 0);
    }
}
