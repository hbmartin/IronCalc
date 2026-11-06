//! Conservative dependency invalidation, adapted from upstream PR #1435.
//! Demand evaluation decides runtime cycles; range edges never imply a cycle.
use crate::{
    expressions::{parser::Node, types::CellReferenceIndex},
    functions::Function,
    model::Model,
    types::{CalculationFailure, CalculationReport, Cell},
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
    by_column: HashMap<(u32, i32), Vec<IndexedSpan>>,
    wide: Vec<(Rectangle, usize)>,
    ranges: Vec<Vec<Key>>,
    volatile: HashSet<Key>,
    has_arrays: bool,
}
/// Detect time/randomness that must not change between fixed-point passes.
pub(crate) fn has_time_or_random(node: &Node) -> bool {
    match node {
        Node::FunctionKind { kind, args } => {
            matches!(
                kind,
                Function::Now
                    | Function::Today
                    | Function::Rand
                    | Function::Randbetween
                    | Function::Randarray
            ) || args.iter().any(has_time_or_random)
        }
        Node::OpRangeKind { left, right }
        | Node::OpConcatenateKind { left, right }
        | Node::OpSumKind { left, right, .. }
        | Node::OpProductKind { left, right, .. }
        | Node::OpPowerKind { left, right }
        | Node::CompareKind { left, right, .. } => {
            has_time_or_random(left) || has_time_or_random(right)
        }
        Node::LambdaDefKind { body, .. } => has_time_or_random(body),
        Node::LambdaCallKind { lambda, args } => {
            has_time_or_random(lambda) || args.iter().any(has_time_or_random)
        }
        Node::NamedFunctionKind { args, .. } => args.iter().any(has_time_or_random),
        Node::ImplicitIntersection { child, .. }
        | Node::SpillRangeOperator { child }
        | Node::UnaryKind { right: child, .. } => has_time_or_random(child),
        _ => false,
    }
}

fn is_volatile(kind: &Function) -> bool {
    matches!(
        kind,
        Function::Now
            | Function::Today
            | Function::Rand
            | Function::Randbetween
            | Function::Randarray
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

impl DependencyIndex {
    fn dependents(&self, cell: Key, out: &mut Vec<Key>) {
        let (sheet, row, column) = cell;
        if let Some(deps) = self.by_cell.get(&cell) {
            out.extend(deps);
        }
        if let Some(ranges) = self.by_column.get(&(sheet, column)) {
            for &(first, last, id) in ranges {
                if first <= row && row <= last {
                    out.extend(&self.ranges[id]);
                }
            }
        }
        for &((s, r1, c1, r2, c2), id) in &self.wide {
            if s == sheet && r1 <= row && row <= r2 && c1 <= column && column <= c2 {
                out.extend(&self.ranges[id]);
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
        let mut index = DependencyIndex::default();
        let mut rectangles: HashMap<Rectangle, usize> = HashMap::new();
        for (sheet, worksheet) in self.workbook.worksheets.iter().enumerate() {
            for (row, column, cell) in worksheet.sheet_data.cells() {
                index.has_arrays |= matches!(
                    cell.as_ref(),
                    Cell::ArrayFormula { .. } | Cell::SpillCell { .. }
                );
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
        self.dependency_index = Some(Arc::new(index));
    }

    /// Recalculate after edits to cell values/formulas. Structural changes must
    /// use full evaluate. `None` means a safe full recalculation was performed.
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
        if self.workbook.settings.iterative_calculation.enabled
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
        while let Some(cell) = queue.pop_front() {
            if !seen.insert(cell) {
                continue;
            }
            next.clear();
            index.dependents(cell, &mut next);
            queue.extend(next.iter().copied().filter(|d| !seen.contains(d)));
        }
        let mut dirty: Vec<_> = seen
            .into_iter()
            .filter(|c| index.formulas.contains_key(c))
            .collect();
        dirty.sort_unstable();
        for cell in &dirty {
            self.evaluation.cells.remove(cell);
            self.links.remove(cell);
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
                .is_some_and(|cell| {
                    matches!(
                        cell.as_ref(),
                        Cell::ArrayFormula { .. } | Cell::SpillCell { .. }
                    )
                })
        });
        if self.evaluation.restart.is_some()
            || !self.evaluation.circular.is_empty()
            || became_array
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
        Some(dirty)
    }
}
