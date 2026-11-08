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
    types::{ArrayKind, Cell, FormulaValue, Link, MergedCell},
    Model,
};

pub(crate) type Position = (u32, i32, i32);
#[cfg(test)]
thread_local! { static CALCULATION_OBSERVER_VISITS: std::cell::Cell<usize> = const {std::cell::Cell::new(0)}; }
#[cfg(test)]
pub(crate) fn calculation_observer_visits() -> usize {
    CALCULATION_OBSERVER_VISITS.with(std::cell::Cell::get)
}

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
        if self.previous_depth == 0 {
            self.target.edit_model().finish_display_activity();
        }
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
    pub(crate) ownership_groups: usize,
    #[cfg(test)]
    pub(crate) sheet_lookup_builds: std::cell::Cell<usize>,
    #[cfg(test)]
    pub(crate) link_cleanup_visits: usize,
    #[cfg(test)]
    pub(crate) incremental_edit_allocations: usize,
    #[cfg(test)]
    pub(crate) clear_scan_visits: usize,
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

/// Why a storage write occurs, independent of a formula's cached value.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) enum WriteIntent {
    #[default]
    Edit,
    Recreate,
    PreserveOwner,
    Calculation,
    Style,
    SpillShape,
}

/// Owners which ordinary replay can leave at a position before its supplement.
#[derive(Clone, Default, Debug)]
struct ReplayOwners {
    undo: Vec<Position>,
    redo: Vec<Position>,
}
impl ReplayOwners {
    fn add(&mut self, owner: Position, undo: bool) {
        let owners = if undo { &mut self.undo } else { &mut self.redo };
        if !owners.contains(&owner) {
            owners.push(owner);
        }
    }
    fn both(&mut self, owner: Position) {
        self.add(owner, true);
        self.add(owner, false);
    }
    fn permits(&self, owner: Position, undo: bool) -> bool {
        (if undo { &self.undo } else { &self.redo }).contains(&owner)
    }
    fn extend(&mut self, other: &Self) {
        for &owner in &other.undo {
            self.add(owner, true);
        }
        for &owner in &other.redo {
            self.add(owner, false);
        }
    }
    fn is_empty(&self) -> bool {
        self.undo.is_empty() && self.redo.is_empty()
    }
    fn iter(&self) -> impl Iterator<Item = &Position> {
        self.undo.iter().chain(&self.redo)
    }
}

/// A single operation resolves stable sheet identities only once.
struct DisplaySheets(HashMap<u32, usize>);
impl DisplaySheets {
    fn index(&self, id: u32) -> Option<usize> {
        self.0.get(&id).copied()
    }
    fn cell<'m>(&self, model: &'m Model<'_>, p: Position) -> Option<std::borrow::Cow<'m, Cell>> {
        model
            .workbook
            .worksheets
            .get(self.index(p.0)?)?
            .cell(p.1, p.2)
    }
}

type FormulaIdentity = Option<(i32, Option<ArrayKind>)>;
#[derive(Debug)]
struct JournalCell {
    before: Option<Cell>,
    expected_formula: FormulaIdentity,
    compare_formula: bool,
    replay: bool,
    owners: Option<Box<ReplayOwners>>,
}
impl JournalCell {
    fn owners_mut(&mut self) -> &mut ReplayOwners {
        self.owners.get_or_insert_with(Default::default)
    }
    fn new(cell: Option<&Cell>) -> Self {
        Self {
            before: cell.cloned(),
            expected_formula: formula_identity(cell),
            compare_formula: false,
            replay: false,
            owners: None,
        }
    }
}

/// One original image and ownership record per touched position.
#[derive(Default, Debug)]
pub(crate) struct MutationJournal {
    cells: HashMap<Position, JournalCell>,
    links: HashMap<Position, Option<Link>>,
    replay_links: HashSet<Position>,
    intent: WriteIntent,
    history: bool,
    content_written: bool,
    ownership_groups: HashMap<(Position, i32, i32), bool>,
}

impl MutationJournal {
    fn capture_group(
        &mut self,
        anchor: Position,
        extent: (i32, i32),
        replay: bool,
        cell_at: impl Fn(Position) -> Option<Cell>,
    ) {
        if !self.history {
            return;
        }
        let key = (anchor, extent.0, extent.1);
        if self
            .ownership_groups
            .get(&key)
            .is_some_and(|&marked| marked || !replay)
        {
            return;
        }
        self.ownership_groups.insert(key, replay);
        let array = matches!(
            self.cells.get(&anchor).and_then(|e| e.before.as_ref()),
            Some(Cell::ArrayFormula { .. })
        ) || matches!(cell_at(anchor), Some(Cell::ArrayFormula { .. }));
        for row in anchor.1..anchor.1.saturating_add(extent.1) {
            for column in anchor.2..anchor.2.saturating_add(extent.0) {
                let p = (anchor.0, row, column);
                let entry = self
                    .cells
                    .entry(p)
                    .or_insert_with(|| JournalCell::new(cell_at(p).as_ref()));
                entry.replay |= replay;
                if replay && array {
                    entry.owners_mut().both(anchor);
                }
            }
        }
        if replay {
            self.replay_links.insert(anchor);
        }
    }
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
    pub(crate) fn write_scope(&self, intent: WriteIntent) -> WriteIntentGuard {
        WriteIntentGuard::new(self.journal.clone(), intent)
    }
    pub(crate) fn capture(
        &self,
        row: i32,
        column: i32,
        cell: Option<&Cell>,
        data: &crate::sheet_data::SheetData,
    ) {
        let Some(journal) = &self.journal else { return };
        let mut records = journal.lock().unwrap_or_else(|e| e.into_inner());
        let intent = records.intent;
        #[cfg(test)]
        if intent == WriteIntent::Calculation {
            CALCULATION_OBSERVER_VISITS.with(|n| n.set(n.get() + 1));
        }
        if intent == WriteIntent::Calculation && !records.history {
            return;
        }
        let entry = records
            .cells
            .entry((self.sheet_id, row, column))
            .or_insert_with(|| JournalCell::new(cell));
        // Check earlier mutable-cell writes before a new scope takes over.
        if entry.compare_formula && entry.expected_formula != formula_identity(cell) {
            entry.replay = true;
        }
        entry.expected_formula = formula_identity(cell);
        entry.compare_formula = matches!(intent, WriteIntent::Edit | WriteIntent::Recreate);
        if records.history
            && matches!(
                intent,
                WriteIntent::Edit | WriteIntent::Recreate | WriteIntent::SpillShape
            )
        {
            let anchor = match cell {
                Some(Cell::ArrayFormula { .. }) => Some((row, column)),
                Some(Cell::SpillCell { a, .. }) => Some(*a),
                _ => None,
            };
            if let Some((r, c)) = anchor {
                if let Some(Cell::ArrayFormula { r: extent, .. }) = data.cell(r, c).as_deref() {
                    let force = intent != WriteIntent::Edit;
                    records.capture_group((self.sheet_id, r, c), *extent, force, |p| {
                        data.cell(p.1, p.2).map(std::borrow::Cow::into_owned)
                    });
                }
            }
        }
    }
    pub(crate) fn removal(
        &self,
        row: i32,
        column: i32,
        old: Option<&Cell>,
        data: &crate::sheet_data::SheetData,
    ) {
        self.replacement(row, column, old, &Cell::default(), data);
    }
    pub(crate) fn replacement(
        &self,
        row: i32,
        column: i32,
        old: Option<&Cell>,
        new: &Cell,
        data: &crate::sheet_data::SheetData,
    ) {
        self.capture(row, column, old, data);
        let Some(journal) = &self.journal else { return };
        let mut records = journal.lock().unwrap_or_else(|e| e.into_inner());
        let intent = records.intent;
        if !matches!(intent, WriteIntent::Calculation | WriteIntent::Style)
            && !same_cell_content(old, Some(new))
        {
            records.content_written = true;
        }
        let Some(entry) = records.cells.get_mut(&(self.sheet_id, row, column)) else {
            return;
        };
        let identity = formula_identity(Some(new));
        if identity.is_some()
            && (intent == WriteIntent::Recreate
                || (intent == WriteIntent::Edit && identity != formula_identity(old)))
        {
            entry.replay = true;
        }
        entry.expected_formula = identity;
        entry.compare_formula = false;
    }
}
fn formula_identity(cell: Option<&Cell>) -> FormulaIdentity {
    match cell? {
        Cell::CellFormula { f, .. } => Some((*f, None)),
        Cell::ArrayFormula { f, kind, .. } => Some((*f, Some(kind.clone()))),
        _ => None,
    }
}

/// Nested write intents restore their predecessor during normal return or unwind.
#[must_use]
pub(crate) struct WriteIntentGuard(Option<Arc<Mutex<MutationJournal>>>, WriteIntent);
impl WriteIntentGuard {
    fn new(journal: Option<Arc<Mutex<MutationJournal>>>, intent: WriteIntent) -> Self {
        let previous = journal.as_ref().map_or(WriteIntent::Edit, |journal| {
            let mut records = journal.lock().unwrap_or_else(|e| e.into_inner());
            std::mem::replace(&mut records.intent, intent)
        });
        Self(journal, previous)
    }
}
impl Drop for WriteIntentGuard {
    fn drop(&mut self) {
        if let Some(journal) = &self.0 {
            journal.lock().unwrap_or_else(|e| e.into_inner()).intent = self.1;
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
    owners: HashMap<Position, ReplayOwners>,
}

impl Model<'_> {
    fn display_sheets(&self) -> DisplaySheets {
        #[cfg(test)]
        self.evaluation
            .display
            .sheet_lookup_builds
            .set(self.evaluation.display.sheet_lookup_builds.get() + 1);
        DisplaySheets(
            self.workbook
                .worksheets
                .iter()
                .enumerate()
                .map(|(i, ws)| (ws.sheet_id, i))
                .collect(),
        )
    }

    /// Ownership metadata can affect calculation without writing a cell.
    pub(crate) fn replace_merged_cells(
        &mut self,
        sheet: u32,
        merges: Vec<MergedCell>,
    ) -> Result<bool, String> {
        if self.workbook.worksheet(sheet)?.merged_cells == merges {
            return Ok(false);
        }
        if let Some(journal) = &self.evaluation.display.journal {
            journal
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .content_written = true;
        }
        self.workbook.worksheet_mut(sheet)?.merged_cells = merges;
        self.invalidate_calculation();
        Ok(true)
    }

    pub(crate) fn reading_stored_values(&self) -> bool {
        self.evaluation.display.stored_reads.load(Ordering::Relaxed)
    }

    #[must_use]
    pub(crate) fn start_stored_reads(&self) -> StoredReadsGuard {
        let flag = Arc::clone(&self.evaluation.display.stored_reads);
        let previous = flag.swap(true, Ordering::Relaxed);
        StoredReadsGuard(flag, previous)
    }

    pub(crate) fn observe_writes(&self, intent: WriteIntent) -> WriteIntentGuard {
        WriteIntentGuard::new(self.evaluation.display.journal.clone(), intent)
    }

    #[cfg(test)]
    pub(crate) fn active_ownership_groups(&self) -> usize {
        self.evaluation.display.journal.as_ref().map_or(0, |j| {
            j.lock()
                .unwrap_or_else(|e| e.into_inner())
                .ownership_groups
                .len()
        })
    }

    pub(crate) fn observe_calculation_writes(&self) -> WriteIntentGuard {
        self.observe_writes(WriteIntent::Calculation)
    }

    pub(crate) fn start_display_journal(&mut self) {
        self.start_display_activity();
        if let Some(journal) = &self.evaluation.display.journal {
            journal.lock().unwrap_or_else(|e| e.into_inner()).history = true;
        }
    }

    fn start_display_activity(&mut self) {
        if self.evaluation.display.journal.is_some() {
            return;
        }
        let journal = Arc::new(Mutex::new(MutationJournal::default()));
        for ws in &mut self.workbook.worksheets {
            ws.sheet_data.observe(MutationObserver {
                sheet_id: ws.sheet_id,
                journal: Some(Arc::clone(&journal)),
            });
        }
        self.evaluation.display.journal = Some(journal);
    }

    fn detach_display_activity(&mut self) {
        self.evaluation.display.journal = None;
        for ws in &mut self.workbook.worksheets {
            ws.sheet_data.observe(MutationObserver::default());
        }
    }

    fn finish_display_activity(&mut self) {
        if self
            .evaluation
            .display
            .journal
            .as_ref()
            .is_some_and(|j| !j.lock().unwrap_or_else(|e| e.into_inner()).history)
        {
            self.detach_display_activity();
        }
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
                records
                    .cells
                    .entry((ws.sheet_id, row, col))
                    .or_insert_with(|| JournalCell::new(None));
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
        let records = journal.lock().unwrap_or_else(|e| e.into_inner());
        #[cfg(test)]
        {
            self.evaluation.display.journal_positions = records.cells.len();
        }
        let sheets = self.display_sheets();
        let owners: Vec<_> = records
            .cells
            .iter()
            .filter_map(|(&p, entry)| {
                let current = sheets.cell(self, p);
                ((entry.replay
                    && (formula_identity(entry.before.as_ref()).is_some()
                        || formula_identity(current.as_deref()).is_some()))
                    || (entry.compare_formula
                        && entry.expected_formula != formula_identity(current.as_deref())))
                .then_some(p)
            })
            .collect();
        drop(records);
        for position in owners {
            self.retain_display_ownership(&journal, position, &sheets);
        }
        let mut records = journal.lock().unwrap_or_else(|e| e.into_inner());
        #[cfg(test)]
        {
            self.evaluation.display.ownership_groups = records.ownership_groups.len();
        }
        let mut replay_owners = HashMap::new();
        let cells = records
            .cells
            .drain()
            .filter_map(|(p, mut entry)| {
                let before = entry.before.as_ref().and_then(CachedCell::of);
                let after = sheets.cell(self, p).as_deref().and_then(CachedCell::of);
                for (cached, undo) in [(&before, false), (&after, true)] {
                    if let Some(CachedCell::Spill(Cell::SpillCell { a, .. })) = cached {
                        entry.owners_mut().add((p.0, a.0, a.1), undo);
                    }
                }
                if before != after || entry.replay {
                    if let Some(owners) = entry.owners {
                        if !owners.is_empty() {
                            replay_owners.insert(p, *owners);
                        }
                    }
                    Some((p, before, after))
                } else {
                    None
                }
            })
            .collect();
        let replay_links = std::mem::take(&mut records.replay_links);
        for &p in &replay_links {
            records.links.entry(p).or_insert_with(|| {
                sheets
                    .index(p.0)
                    .and_then(|i| self.links.get(&(i as u32, p.1, p.2)))
                    .cloned()
            });
        }
        let links = records
            .links
            .drain()
            .filter_map(|(p, before)| {
                let after = sheets
                    .index(p.0)
                    .and_then(|i| self.links.get(&(i as u32, p.1, p.2)))
                    .cloned();
                (before != after || replay_links.contains(&p)).then_some((p, before, after))
            })
            .collect();
        DisplayDelta {
            cells,
            links,
            owners: replay_owners,
        }
    }

    pub(crate) fn prepared_display_cells(&self) -> Vec<(u32, i32, i32, Option<Cell>)> {
        let Some(journal) = &self.evaluation.display.journal else {
            return Vec::new();
        };
        let sheets = self.display_sheets();
        journal
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .cells
            .keys()
            .filter_map(|&(id, r, c)| {
                let sheet = sheets.index(id)? as u32;
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
        let sheets = self.display_sheets();
        journal
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .links
            .keys()
            .filter_map(|&(id, r, c)| {
                let sheet = sheets.index(id)? as u32;
                let p = (sheet, r, c);
                Some((p, self.links.get(&p).cloned()))
            })
            .collect()
    }
    /// Publication marks already captured original groups and new extents.
    fn retain_display_ownership(
        &self,
        journal: &Arc<Mutex<MutationJournal>>,
        position: Position,
        sheets: &DisplaySheets,
    ) {
        let mut records = journal.lock().unwrap_or_else(|e| e.into_inner());
        if !records.history {
            return;
        }
        let current = sheets.cell(self, position);
        let original = records.cells.get(&position).and_then(|e| e.before.as_ref());
        let mut extents = vec![(1, 1)];
        for cell in original.into_iter().chain(current.as_deref()) {
            if let Cell::ArrayFormula { r, .. } = cell {
                extents.push(*r);
            }
        }
        extents.sort_unstable();
        extents.dedup();
        for extent in extents {
            // Storage must retain the original group before destructive edits.
            if let Some(Cell::ArrayFormula { r, .. }) =
                records.cells.get(&position).and_then(|e| e.before.as_ref())
            {
                debug_assert!(
                    records.ownership_groups.contains_key(&(position, r.0, r.1))
                        || records
                            .cells
                            .get(&position)
                            .is_some_and(|e| e.owners.is_some())
                );
            }
            records.capture_group(position, extent, true, |p| {
                sheets.cell(self, p).map(std::borrow::Cow::into_owned)
            });
        }
        records.links.entry(position).or_insert_with(|| {
            sheets
                .index(position.0)
                .and_then(|s| self.links.get(&(s as u32, position.1, position.2)))
                .cloned()
        });
    }

    fn mark_replay_positions(
        &self,
        positions: impl IntoIterator<Item = Position>,
        sheets: &DisplaySheets,
    ) {
        if let Some(journal) = &self.evaluation.display.journal {
            let mut records = journal.lock().unwrap_or_else(|e| e.into_inner());
            if !records.history {
                return;
            }
            for p in positions {
                records
                    .cells
                    .entry(p)
                    .or_insert_with(|| {
                        let cell = sheets.cell(self, p);
                        JournalCell::new(cell.as_deref())
                    })
                    .replay = true;
            }
        }
    }

    pub(crate) fn retain_prepared_replay(&self, delta: &DisplayDelta) {
        let sheets = self.display_sheets();
        self.mark_replay_positions(delta.cells.iter().map(|(p, _, _)| *p), &sheets);
        if let Some(journal) = &self.evaluation.display.journal {
            let mut records = journal.lock().unwrap_or_else(|e| e.into_inner());
            if !records.history {
                return;
            }
            for (p, owners) in &delta.owners {
                records
                    .cells
                    .entry(*p)
                    .or_insert_with(|| JournalCell::new(sheets.cell(self, *p).as_deref()))
                    .owners_mut()
                    .extend(owners);
            }
            records
                .replay_links
                .extend(delta.links.iter().map(|(p, _, _)| *p));
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
        if self.evaluation.display.edit_depth == 0 {
            self.start_display_activity();
        }
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
        let requested = std::mem::take(&mut self.evaluation.display.calculation_requested);
        let formatting = std::mem::take(&mut self.evaluation.display.formatting_requested);
        let sheets = self.display_sheets();
        let touched = (!recover || (!requested && !formatting))
            && self
                .evaluation
                .display
                .journal
                .as_ref()
                .is_some_and(|journal| {
                    let records = journal.lock().unwrap_or_else(|e| e.into_inner());
                    records.content_written
                        || records.cells.iter().any(|(p, entry)| {
                            let current = sheets.cell(self, *p);
                            !same_cell_content(entry.before.as_ref(), current.as_deref())
                        })
                });
        if !recover && touched {
            self.invalidate_calculation();
        }
        // Rejected no-op edits have no orphan owners or overlays to reconcile.
        if !requested && !formatting && !touched {
            if recover && self.evaluation.display.formatting_recovery {
                self.evaluate_conditional_formatting();
            }
            return;
        }
        let positions: Vec<_> = if let Some(journal) = &self.evaluation.display.journal {
            let records = journal.lock().unwrap_or_else(|e| e.into_inner());
            records
                .cells
                .keys()
                .filter_map(|p| sheets.index(p.0).map(|i| (i as u32, p.1, p.2)))
                .collect()
        } else {
            debug_assert!(false, "an outer display edit requires mutation observation");
            Vec::new()
        };
        for position in positions {
            #[cfg(test)]
            {
                self.evaluation.display.link_cleanup_visits += 1;
            }
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
        if calculate && requested {
            self.finish_display_activity();
            self.evaluate();
        } else if formatting
            || (!recover && touched)
            || (recover && self.evaluation.display.formatting_recovery)
        {
            self.evaluate_conditional_formatting();
        }
    }

    pub(crate) fn abort_display_edit(&mut self, previous_depth: usize) {
        self.evaluation.display.edit_depth = previous_depth;
        self.detach_display_activity();
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
        let _writes = self.observe_writes(WriteIntent::PreserveOwner);
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
        let sheets = self.display_sheets();
        // A later calculation can grow another retained anchor without an explicit
        // edit at its cells. Preserve that group's current shape and values as a
        // unit, rather than restoring an old extent over its new children.
        let protected: HashSet<_> = delta
            .cells
            .iter()
            .filter_map(|(p, before, after)| {
                let current = sheets.cell(self, *p);
                let Some(Cell::ArrayFormula { r, .. }) = current.as_deref() else {
                    return None;
                };
                (*r != (1, 1)
                    && ![before, after].into_iter().flatten().any(
                        |c| matches!(c, CachedCell::Formula(_, Some((_, extent))) if extent == r),
                    ))
                .then_some(*p)
            })
            .collect();
        let mut cells: Vec<_> = delta.cells.iter().collect();
        cells.sort_unstable_by_key(|(key, before, after)| {
            let rank = match if undo { before } else { after } {
                None => 0,
                Some(CachedCell::Formula(..)) => 1,
                Some(CachedCell::Spill(..)) => 2,
            };
            (rank, *key)
        });
        for (key, before, after) in cells {
            let Some(sheet) = sheets.index(key.0) else {
                continue;
            };
            if protected.contains(key) {
                continue;
            }
            let current_owner = sheets.cell(self, *key).as_deref().and_then(|c| {
                if let Cell::SpillCell { a, .. } = c {
                    Some((key.0, a.0, a.1))
                } else {
                    None
                }
            });
            let target = if undo { before } else { after };
            if current_owner.is_some_and(|p| protected.contains(&p))
                || matches!(target, Some(CachedCell::Spill(Cell::SpillCell { a, .. })) if protected.contains(&(key.0, a.0, a.1)))
            {
                continue;
            }
            if let Some(owner) = current_owner {
                let same_target = matches!(target, Some(CachedCell::Spill(Cell::SpillCell { a, .. })) if owner == (key.0, a.0, a.1));
                if !same_target
                    && !delta
                        .owners
                        .get(key)
                        .is_some_and(|owners| owners.permits(owner, undo))
                {
                    continue;
                }
            }
            match target {
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
            if protected.contains(key) {
                continue;
            }
            let Some(sheet) = sheets.index(key.0) else {
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
        let sheets = self.display_sheets();
        // Ownership moves need before/after entries even when their payloads compare equal.
        self.mark_replay_positions(
            before.cells.keys().flat_map(|&p| {
                (map(p) != Some(p))
                    .then_some([Some(p), map(p)])
                    .into_iter()
                    .flatten()
                    .flatten()
            }),
            &sheets,
        );
        if let Some(journal) = &self.evaluation.display.journal {
            let mut records = journal.lock().unwrap_or_else(|e| e.into_inner());
            for &p in before.links.keys() {
                if map(p) != Some(p) {
                    records.replay_links.insert(p);
                    if let Some(target) = map(p) {
                        records.replay_links.insert(target);
                    }
                }
            }
        }
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
        // Capture transient ownership, including identity children and vacant targets.
        if let Some(journal) = &self.evaluation.display.journal {
            let mut records = journal.lock().unwrap_or_else(|e| e.into_inner());
            if records.history {
                for (&anchor, cached) in &before.cells {
                    if let CachedCell::Formula(_, Some((_, (w, h)))) = cached {
                        let target = map(anchor);
                        if target == Some(anchor) && groups.get(&anchor).is_some_and(|g| g.1) {
                            continue;
                        }
                        for r in anchor.1..anchor.1.saturating_add(*h) {
                            for c in anchor.2..anchor.2.saturating_add(*w) {
                                let p = (anchor.0, r, c);
                                for position in [Some(p), map(p)].into_iter().flatten() {
                                    let entry =
                                        records.cells.entry(position).or_insert_with(|| {
                                            JournalCell::new(sheets.cell(self, position).as_deref())
                                        });
                                    entry.replay = true;
                                    entry.owners_mut().both(anchor);
                                    if let Some(owner) = target {
                                        entry.owners_mut().both(owner);
                                    }
                                }
                            }
                        }
                    }
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
                    if let Some(sheet) = sheets.index(candidate.0) {
                        let ws = &mut self.workbook.worksheets[sheet];
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
            let Some(sheet) = sheets.index(target.0) else {
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
                        sheet as u32,
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
                                sheet as u32,
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
                if let Some(sheet) = sheets.index(target.0) {
                    if self.workbook.worksheets[sheet]
                        .cell(target.1, target.2)
                        .is_some_and(|c| c.has_formula())
                    {
                        self.links.insert((sheet as u32, target.1, target.2), link);
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
        let _writes = self.observe_writes(WriteIntent::SpillShape);
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
        for (p, authorized) in &self.owners {
            if conflicts.contains(*p) || authorized.iter().any(|p| conflicts.contains(*p)) {
                owners.extend(authorized.iter().copied());
            }
        }
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
            if self.owners.get(position).is_some_and(|o| {
                o.iter()
                    .any(|p| conflicts.contains(*p) || owners.contains(p))
            }) {
                return false;
            }
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
        let positions: HashSet<_> = self.cells.iter().map(|(p, _, _)| *p).collect();
        self.owners.retain(|p, _| positions.contains(p));
    }
    pub(crate) fn intersects(&self, conflicts: &DisplayConflicts) -> bool {
        self.cells.iter().any(|(p, before, after)| {
            conflicts.contains(*p)
                || self
                    .owners
                    .get(p)
                    .is_some_and(|o| o.iter().any(|p| conflicts.contains(*p)))
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

fn same_cell_content(before: Option<&Cell>, after: Option<&Cell>) -> bool {
    if before == after {
        return true;
    }
    if let (Some(before), Some(after)) = (before, after) {
        if before.get_style() == after.get_style() {
            return false;
        }
    }
    let normalized = |cell: Option<&Cell>| {
        cell.filter(|c| !matches!(c, Cell::EmptyCell { .. }))
            .map(|cell| {
                let mut cell = cell.clone();
                cell.set_style(0);
                cell
            })
    };
    normalized(before) == normalized(after)
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
            ..Default::default()
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

#[cfg(test)]
mod ownership_tests {
    #![allow(clippy::unwrap_used, clippy::panic)]
    use super::*;
    use crate::{test::util::new_empty_model, types::SpillValue};

    #[test]
    fn write_intents_are_explicit_and_storage_preserves_the_caller() {
        let mut raw = new_empty_model();
        raw._set("A1", "=1");
        raw.start_display_journal();
        let journal = raw.evaluation.display.journal.clone().unwrap();
        let outer = raw.observe_writes(WriteIntent::PreserveOwner);
        raw.workbook.worksheets[0]
            .set_cell_with_formula(1, 1, 0, 0)
            .unwrap();
        assert_eq!(journal.lock().unwrap().intent, WriteIntent::PreserveOwner);
        {
            let _inner = raw.observe_writes(WriteIntent::Recreate);
            assert_eq!(journal.lock().unwrap().intent, WriteIntent::Recreate);
        }
        assert_eq!(journal.lock().unwrap().intent, WriteIntent::PreserveOwner);
        drop(outer);
        assert_eq!(journal.lock().unwrap().intent, WriteIntent::Edit);
    }

    #[cfg(panic = "unwind")]
    #[test]
    fn explicit_write_intents_restore_on_unwind() {
        let mut raw = new_empty_model();
        raw.start_display_journal();
        let journal = raw.evaluation.display.journal.clone().unwrap();
        let _outer = raw.observe_writes(WriteIntent::SpillShape);
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _inner = raw.observe_writes(WriteIntent::Recreate);
            panic!("intent scope");
        }));
        assert!(result.is_err());
        assert_eq!(journal.lock().unwrap().intent, WriteIntent::SpillShape);
    }

    #[test]
    fn absent_and_retained_children_preserve_foreign_spill_owners() {
        for undo in [false, true] {
            for restore in [false, true] {
                let mut raw = new_empty_model();
                raw._set("A1", "=SEQUENCE(1,2)");
                raw.evaluate();
                let id = raw.workbook.worksheets[0].sheet_id;
                let key = (id, 1, 2);
                let replacement = restore.then_some(CachedCell::Spill(Cell::SpillCell {
                    s: 0,
                    a: (3, 1),
                    v: SpillValue::Number(99.0),
                }));
                let mut owners = ReplayOwners::default();
                owners.both((id, 3, 1));
                let delta = DisplayDelta {
                    cells: vec![(key, replacement.clone(), replacement)],
                    owners: HashMap::from([(key, owners)]),
                    ..Default::default()
                };
                let before = raw.to_bytes();
                raw.apply_display_delta(&delta, undo);
                assert_eq!(raw.to_bytes(), before);
            }
        }
    }

    #[test]
    fn vacant_removals_are_direction_specific_and_keep_authorized_cleanup() {
        for undo in [false, true] {
            let mut raw = new_empty_model();
            raw._set("A1", "=SEQUENCE(1,2)");
            raw.evaluate();
            let id = raw.workbook.worksheets[0].sheet_id;
            let key = (id, 1, 2);
            let mut owners = ReplayOwners::default();
            owners.add((id, 1, 1), undo);
            let delta = DisplayDelta {
                cells: vec![(key, None, None)],
                owners: HashMap::from([(key, owners)]),
                ..Default::default()
            };
            let before = raw.to_bytes();
            raw.apply_display_delta(&delta, !undo);
            assert_eq!(raw.to_bytes(), before);
            raw.apply_display_delta(&delta, undo);
            assert!(!matches!(
                raw.workbook.worksheets[0].cell(1, 2).as_deref(),
                Some(Cell::SpillCell { .. })
            ));
        }
    }

    #[test]
    fn a_later_extent_preserves_its_children_and_dynamic_link() {
        for undo in [false, true] {
            let mut raw = new_empty_model();
            raw._set("A1", "=SEQUENCE(1,5)");
            raw.evaluate();
            let id = raw.workbook.worksheets[0].sheet_id;
            let anchor = (id, 1, 1);
            let link = Link::External {
                target: "https://example.com/current".into(),
                tooltip: None,
            };
            raw.insert_dynamic_link((0, 1, 1), link.clone());
            let old = CachedCell::Formula(
                FormulaValue::Number(1.0),
                Some((ArrayKind::Dynamic, (1, 1))),
            );
            let delta = DisplayDelta {
                cells: vec![(anchor, Some(old.clone()), Some(old))],
                links: vec![(anchor, None, None)],
                ..Default::default()
            };
            let bytes = raw.to_bytes();
            raw.apply_display_delta(&delta, undo);
            assert_eq!(raw.to_bytes(), bytes);
            assert_eq!(raw.links.get(&(0, 1, 1)), Some(&link));
        }
    }

    #[test]
    fn vacant_owner_conflicts_prune_the_complete_supplement() {
        let id = 1;
        let anchor = (id, 1, 1);
        let vacant = (id, 3, 2);
        let mut owners = ReplayOwners::default();
        owners.both(anchor);
        let mut delta = DisplayDelta {
            cells: vec![
                (
                    anchor,
                    Some(CachedCell::Formula(
                        FormulaValue::Number(1.0),
                        Some((ArrayKind::Dynamic, (2, 2))),
                    )),
                    None,
                ),
                (vacant, None, None),
            ],
            owners: HashMap::from([(vacant, owners)]),
            ..Default::default()
        };
        let conflicts = DisplayConflicts {
            areas: HashSet::from([(id, 3, 2, 1, 1)]),
            ..Default::default()
        };
        assert!(delta.intersects(&conflicts));
        delta.discard_conflicts(&conflicts);
        assert!(delta.cells.is_empty());
        assert!(delta.owners.is_empty());
    }

    #[test]
    fn storage_captures_equal_array_children_before_mutable_replacement() {
        for operation in 0..4 {
            let mut raw = new_empty_model();
            raw._set("B2", "=SEQUENCE(2,2)");
            raw._set("F6", "=SEQUENCE(2,2)+0");
            raw.evaluate();
            let f = raw.workbook.worksheets[0]
                .cell(6, 6)
                .unwrap()
                .get_formula()
                .unwrap();
            raw.start_display_journal();
            let mut cell = raw.workbook.worksheets[0].cell(2, 2).unwrap().into_owned();
            if let Cell::ArrayFormula { f: index, .. } = &mut cell {
                *index = f;
            }
            let data = &mut raw.workbook.worksheets[0].sheet_data;
            match operation {
                0 => {
                    data.set_cell(2, 2, cell);
                }
                1 => {
                    *data.cell_mut(2, 2).unwrap() = cell;
                }
                2 => {
                    data.set_row(
                        2,
                        [
                            (2, cell),
                            (
                                3,
                                Cell::SpillCell {
                                    s: 0,
                                    a: (2, 2),
                                    v: SpillValue::Number(2.0),
                                },
                            ),
                        ],
                    );
                }
                _ => {
                    data.remove_cell(2, 2);
                    data.set_cell(2, 2, cell);
                }
            }
            let delta = raw.take_display_journal();
            assert_eq!(delta.cell_count(), 4, "operation={operation}");
            assert_eq!(raw.evaluation.display.ownership_groups, 2);
            assert!(delta
                .cells
                .iter()
                .any(|(p, before, after)| p.1 == 3 && p.2 == 3 && before == after));
        }
    }
}
