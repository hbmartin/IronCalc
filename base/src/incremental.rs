//! Conservative dependency invalidation, adapted from upstream PR #1435.
//! Demand evaluation decides runtime cycles; range edges never imply a cycle.
use crate::{
    evaluation::scalar_calculation_cell,
    expressions::{parser::Node, types::CellReferenceIndex},
    functions::Function,
    model::Model,
    types::{CalculationFailure, CalculationReport},
};
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;

type Key = (u32, i32, i32);
type Rectangle = (u32, i32, i32, i32, i32);
type IndexedSpan = (i32, i32, usize);
#[derive(Clone, Debug)]
enum Ref {
    Cell(u32, i32, i32),
    Range(u32, i32, i32, i32, i32),
}
#[derive(Default)]
pub(crate) struct DependencyIndex {
    formulas: HashMap<Key, Arc<Node>>,
    by_cell: HashMap<Key, Vec<Key>>,
    // Rectangles are deduplicated before column indexing. Thousands of lookups
    // can share one table range without thousands of duplicate containment tests.
    by_column: HashMap<(u32, i32), IntervalIndex>,
    wide: Vec<(Rectangle, usize)>,
    ranges: Vec<Vec<Key>>,
    volatile: HashSet<Key>,
    has_arrays: bool,
}
fn is_volatile(kind: &Function) -> bool {
    kind.is_random()
        || matches!(
            kind,
            Function::Now
                | Function::Today
                | Function::Indirect
                | Function::Offset
                | Function::Cell
                | Function::Info
        )
}

/// Collects what a formula at `cell` refers to. Returns true when the
/// references can only be known by evaluating (the formula is volatile).
fn collect(node: &Node, cell: Key, refs: &mut Vec<Ref>) -> bool {
    let (row0, column0) = (cell.1, cell.2);
    let many = |nodes: &[Node], refs: &mut Vec<Ref>| {
        let mut volatile = false;
        for n in nodes {
            volatile |= collect(n, cell, refs);
        }
        volatile
    };
    match node {
        Node::ReferenceKind {
            sheet_index,
            absolute_row,
            absolute_column,
            row,
            column,
            ..
        } => {
            let r = if *absolute_row { *row } else { row + row0 };
            let c = if *absolute_column {
                *column
            } else {
                column + column0
            };
            refs.push(Ref::Cell(*sheet_index, r, c));
            false
        }
        Node::RangeKind {
            sheet_index,
            absolute_row1,
            absolute_column1,
            row1,
            column1,
            absolute_row2,
            absolute_column2,
            row2,
            column2,
            ..
        } => {
            let r1 = if *absolute_row1 { *row1 } else { row1 + row0 };
            let r2 = if *absolute_row2 { *row2 } else { row2 + row0 };
            let c1 = if *absolute_column1 {
                *column1
            } else {
                column1 + column0
            };
            let c2 = if *absolute_column2 {
                *column2
            } else {
                column2 + column0
            };
            refs.push(Ref::Range(
                *sheet_index,
                r1.min(r2),
                c1.min(c2),
                r1.max(r2),
                c1.max(c2),
            ));
            false
        }
        Node::OpRangeKind { left, right } => {
            // A1:INDEX(...) is only known when evaluated
            collect(left, cell, refs);
            collect(right, cell, refs);
            true
        }
        Node::OpConcatenateKind { left, right }
        | Node::OpSumKind { left, right, .. }
        | Node::OpProductKind { left, right, .. }
        | Node::OpPowerKind { left, right }
        | Node::CompareKind { left, right, .. } => {
            let a = collect(left, cell, refs);
            collect(right, cell, refs) || a
        }
        Node::FunctionKind { kind, args } => many(args, refs) || is_volatile(kind),
        Node::LambdaDefKind { body, .. } => collect(body, cell, refs),
        Node::LambdaCallKind { lambda, args } => {
            let a = collect(lambda, cell, refs);
            many(args, refs) || a
        }
        Node::NamedFunctionKind { args, .. } => {
            many(args, refs);
            true
        }
        // A name or table may point anywhere, and can change
        Node::DefinedNameKind(_) | Node::TableNameKind(_) => true,
        Node::ImplicitIntersection { child, .. }
        | Node::SpillRangeOperator { child }
        | Node::UnaryKind { right: child, .. } => collect(child, cell, refs),
        Node::BooleanKind(_)
        | Node::NumberKind(_)
        | Node::StringKind(_)
        | Node::WrongReferenceKind { .. }
        | Node::WrongRangeKind { .. }
        | Node::ArrayKind(_)
        | Node::NamedVariableKind { .. }
        | Node::ErrorKind(_)
        | Node::ParseErrorKind { .. }
        | Node::EmptyArgKind => false,
    }
}

// A balanced immutable interval tree. Each node caches its subtree's maximum
// endpoint, so queries skip spans that cannot contain the edited row.
#[derive(Default)]
struct IntervalIndex {
    spans: Vec<IndexedSpan>,
    maxima: Vec<i32>,
}
impl IntervalIndex {
    fn push(&mut self, span: IndexedSpan) {
        self.spans.push(span);
    }
    fn finish(&mut self) {
        self.spans.sort_unstable();
        self.maxima.resize(self.spans.len(), 0);
        fn build(spans: &[IndexedSpan], maxima: &mut [i32]) -> i32 {
            if spans.is_empty() {
                return i32::MIN;
            }
            let mid = spans.len() / 2;
            let (left, right) = maxima.split_at_mut(mid);
            let max = spans[mid]
                .1
                .max(build(&spans[..mid], left))
                .max(build(&spans[mid + 1..], &mut right[1..]));
            right[0] = max;
            max
        }
        build(&self.spans, &mut self.maxima);
    }
    fn containing(&self, row: i32, mut emit: impl FnMut(usize)) {
        fn visit(spans: &[IndexedSpan], maxima: &[i32], row: i32, emit: &mut impl FnMut(usize)) {
            if spans.is_empty() {
                return;
            }
            let mid = spans.len() / 2;
            if maxima[mid] < row || spans[0].0 > row {
                return;
            }
            visit(&spans[..mid], &maxima[..mid], row, emit);
            let (first, last, id) = spans[mid];
            if first <= row && row <= last {
                emit(id);
            }
            if first <= row {
                visit(&spans[mid + 1..], &maxima[mid + 1..], row, emit);
            }
        }
        visit(&self.spans, &self.maxima, row, &mut emit);
    }
}

impl DependencyIndex {
    fn dependents(&self, cell: Key, expanded: &mut HashSet<usize>, out: &mut Vec<Key>) {
        let (sheet, row, column) = cell;
        if let Some(deps) = self.by_cell.get(&cell) {
            out.extend(deps);
        }
        let mut emit = |id| {
            if expanded.insert(id) {
                out.extend(&self.ranges[id]);
            }
        };
        if let Some(ranges) = self.by_column.get(&(sheet, column)) {
            ranges.containing(row, &mut emit);
        }
        for &((s, r1, c1, r2, c2), id) in &self.wide {
            if s == sheet && r1 <= row && row <= r2 && c1 <= column && column <= c2 {
                emit(id);
            }
        }
    }
}

impl Model<'_> {
    fn incremental_node(&self, cell: Key) -> Option<Arc<Node>> {
        let formula = self
            .workbook
            .worksheets
            .get(cell.0 as usize)?
            .cell(cell.1, cell.2)?
            .get_formula()?;
        self.parsed_formulas
            .get(cell.0 as usize)?
            .get(formula as usize)
            .map(|(node, _)| Arc::clone(node))
    }

    fn build_dependency_index(&mut self) {
        #[cfg(test)]
        {
            self.evaluation.dependency_index_builds += 1;
        }
        let mut index = DependencyIndex::default();
        let mut rectangles: HashMap<Rectangle, usize> = HashMap::new();
        for (sheet, worksheet) in self.workbook.worksheets.iter().enumerate() {
            for (row, column, cell) in worksheet.sheet_data.cells() {
                index.has_arrays |= !scalar_calculation_cell(&cell);
                let Some(formula) = cell.get_formula() else {
                    continue;
                };
                let Some((node, _)) = self.parsed_formulas[sheet].get(formula as usize) else {
                    continue;
                };
                let key = (sheet as u32, row, column);
                let mut refs = Vec::new();
                if collect(node, key, &mut refs) {
                    index.volatile.insert(key);
                }
                index.formulas.insert(key, Arc::clone(node));
                for reference in refs {
                    match reference {
                        Ref::Cell(s, r, c) => {
                            index.by_cell.entry((s, r, c)).or_default().push(key);
                        }
                        Ref::Range(s, r1, c1, r2, c2) => {
                            let rectangle = (s, r1, c1, r2, c2);
                            let id = if let Some(id) = rectangles.get(&rectangle) {
                                *id
                            } else {
                                let id = index.ranges.len();
                                index.ranges.push(Vec::new());
                                rectangles.insert(rectangle, id);
                                if c2 - c1 < 1024 {
                                    for c in c1..=c2 {
                                        index
                                            .by_column
                                            .entry((s, c))
                                            .or_default()
                                            .push((r1, r2, id));
                                    }
                                } else {
                                    index.wide.push((rectangle, id));
                                }
                                id
                            };
                            index.ranges[id].push(key);
                        }
                    }
                }
            }
        }
        for spans in index.by_column.values_mut() {
            spans.finish();
        }
        self.dependency_index = Some(Arc::new(index));
    }

    /// Recalculate after edits to cell values/formulas, supplying every edited
    /// cell. Structural model APIs automatically invalidate the baseline.
    /// Direct workbook mutations still require a full evaluation.
    /// `None` means the full evaluation path was selected. Inspect
    /// [`Model::evaluation_report`] to determine whether it completed successfully.
    pub fn evaluate_incremental(&mut self, edited: &[(u32, i32, i32)]) -> Option<Vec<Key>> {
        let changed = self.evaluate_incremental_headless(edited);
        self.evaluate_conditional_formatting();
        changed
    }

    /// Incremental calculation without UI caches. Cyclic/array calculations
    /// conservatively retain the full evaluator and its convergence contract.
    pub fn evaluate_incremental_headless(
        &mut self,
        edited: &[(u32, i32, i32)],
    ) -> Option<Vec<Key>> {
        let _clock = self.start_calculation_clock();
        if self.workbook.settings.iterative_calculation.enabled
            || !self.evaluation.report.converged
            || self.evaluation.scalar_only != Some(true)
            || self.evaluation.cells.is_empty()
            || !self.evaluation.circular.is_empty()
        {
            self.evaluate_headless();
            return None;
        }
        if self.dependency_index.is_none() {
            self.build_dependency_index();
        }
        let mut index = Arc::clone(self.dependency_index.as_ref()?);
        let structure_changed =
            edited.iter().any(
                |key| match (index.formulas.get(key), self.incremental_node(*key)) {
                    (None, None) => false,
                    (Some(a), Some(b)) => !Arc::ptr_eq(a, &b),
                    _ => true,
                },
            );
        if structure_changed {
            self.build_dependency_index();
            index = Arc::clone(self.dependency_index.as_ref()?);
        }
        if index.has_arrays {
            self.evaluate_headless();
            return None;
        }
        let mut seen = HashSet::new();
        let mut queue: VecDeque<Key> = edited.iter().copied().collect();
        queue.extend(index.volatile.iter().copied());
        let mut next = Vec::new();
        let mut expanded_ranges = HashSet::new();
        while let Some(cell) = queue.pop_front() {
            if !seen.insert(cell) {
                continue;
            }
            next.clear();
            index.dependents(cell, &mut expanded_ranges, &mut next);
            queue.extend(next.iter().copied().filter(|d| !seen.contains(d)));
        }
        let mut dirty: Vec<_> = seen
            .into_iter()
            .filter(|c| index.formulas.contains_key(c))
            .collect();
        dirty.sort_unstable();
        for cell in &dirty {
            self.evaluation.cells.remove(cell);
            self.remove_dynamic_link(cell);
        }
        for cell in edited {
            self.evaluation.cells.remove(cell);
        }
        self.evaluation.seen.clear();
        self.evaluation.seen_ranges.clear();
        self.evaluation.stack.clear();
        self.evaluation.restart = None;
        self.evaluation.depth = 0;
        self.evaluation.unwinding = false;
        self.evaluation.in_pass = true;
        self.evaluation.recursion_limit_hit = false;
        self.evaluation.recursion_failures.clear();
        self.evaluation.spill_changed = false;
        self.evaluation.lambda_depth = 0;
        self.clear_variable_stack();
        self.clear_lambdas();
        for &(sheet, row, column) in &dirty {
            let cell = CellReferenceIndex { sheet, row, column };
            self.evaluation.root = Some(cell);
            self.evaluate_cell(cell);
            if self.evaluation.restart.is_some() || !self.evaluation.circular.is_empty() {
                break;
            }
        }
        self.evaluation.in_pass = false;
        self.evaluation.root = None;
        let became_array = dirty.iter().any(|&(s, r, c)| {
            self.workbook.worksheets[s as usize]
                .cell(r, c)
                .is_some_and(|cell| !scalar_calculation_cell(&cell))
        });
        if self.evaluation.restart.is_some()
            || !self.evaluation.circular.is_empty()
            || became_array
            || self.evaluation.spill_changed
            || self.evaluation.recursion_limit_hit
        {
            self.evaluate_headless();
            return None;
        }
        self.evaluation.report = CalculationReport {
            converged: true,
            iterations: 1,
            residual: 0.0,
            had_circular_references: false,
            failure: None::<CalculationFailure>,
        };
        self.dependency_index = Some(index);
        self.record_display_timestamp();
        Some(dirty)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interval_queries_match_linear_containment() {
        let mut index = IntervalIndex::default();
        for id in 0..2000 {
            let start = (id * 193 % 4000) as i32;
            index.push((start, start + (id % 19) as i32, id));
        }
        index.finish();
        for row in [-1, 0, 1, 10, 999, 2000, 3999, 5000] {
            let mut actual = Vec::new();
            index.containing(row, |id| actual.push(id));
            actual.sort_unstable();
            let mut expected: Vec<_> = index
                .spans
                .iter()
                .filter(|&&(a, b, _)| a <= row && row <= b)
                .map(|&(_, _, id)| id)
                .collect();
            expected.sort_unstable();
            assert_eq!(actual, expected);
        }
    }

    #[test]
    fn rectangles_expand_once_across_columns_and_reads() {
        let mut index = DependencyIndex::default();
        index.ranges.push(vec![(0, 10, 10)]);
        for col in [1, 2] {
            let spans = index.by_column.entry((0, col)).or_default();
            spans.push((1, 10, 0));
            spans.finish();
        }
        let mut expanded = HashSet::new();
        let mut out = Vec::new();
        for row in 1..=10 {
            for col in [1, 2] {
                index.dependents((0, row, col), &mut expanded, &mut out);
            }
        }
        assert_eq!(out, vec![(0, 10, 10)]);
    }
}
