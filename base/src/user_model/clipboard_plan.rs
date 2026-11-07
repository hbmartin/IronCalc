//! Isolated, sparse preparation. Nothing is published until every operation succeeds.
use super::clipboard::{ClipboardData, ClipboardPastePlan, ClipboardTuple};
use crate::{
    expressions::types::Area,
    types::{Cell, Workbook, Worksheet},
    Model, UserModel,
};
use std::collections::HashSet;

pub(super) struct PreparedClipboard<'a> {
    staged: UserModel<'a>,
    cells: Vec<(u32, i32, i32, Option<Cell>)>,
    replay: crate::display::DisplayDelta,
    links: Vec<((u32, i32, i32), Option<crate::types::Link>)>,
}

impl<'a> UserModel<'a> {
    pub(super) fn prepare_clipboard_commit(
        &self,
        source_sheet: u32,
        source_range: ClipboardTuple,
        clipboard: &ClipboardData,
        is_cut: bool,
        plan: ClipboardPastePlan,
    ) -> Result<PreparedClipboard<'a>, String> {
        let (r1, c1, r2, c2) = source_range;
        let view = self.get_selected_view();
        let mut areas = vec![
            Area {
                sheet: source_sheet,
                row: r1,
                column: c1,
                width: c2 - c1 + 1,
                height: r2 - r1 + 1,
            },
            Area {
                sheet: view.sheet,
                row: view.row,
                column: view.column,
                width: c2 - c1 + 1,
                height: r2 - r1 + 1,
            },
        ];
        for m in self.model.get_merged_cells(source_sheet)? {
            if m.intersects(r1, c1, c2 - c1 + 1, r2 - r1 + 1) {
                let row = m.row + view.row - r1;
                let column = m.column + view.column - c1;
                // Invalid translated merges are skipped by the prepared operation.
                if row > 0
                    && column > 0
                    && row + m.height - 1 <= crate::constants::LAST_ROW
                    && column + m.width - 1 <= crate::constants::LAST_COLUMN
                {
                    areas.push(Area {
                        sheet: view.sheet,
                        row,
                        column,
                        width: m.width,
                        height: m.height,
                    });
                }
            }
        }
        for &(sheet, row, column, _) in &plan.formula_updates {
            areas.push(Area {
                sheet,
                row,
                column,
                width: 1,
                height: 1,
            });
        }
        let mut positions = HashSet::new();
        let mut owners = HashSet::new();
        while let Some(area) = areas.pop() {
            let ws = self.model.workbook.worksheet(area.sheet)?;
            for row in area.row..area.row + area.height {
                for (column, cell) in ws.sheet_data.cells_in_row_between(
                    row,
                    area.column,
                    area.column + area.width - 1,
                ) {
                    positions.insert((area.sheet, row, column));
                    let anchor = match &*cell {
                        Cell::SpillCell { a, .. } => Some(*a),
                        Cell::ArrayFormula { .. } => Some((row, column)),
                        _ => None,
                    };
                    if let Some((r, c)) = anchor {
                        if owners.insert((area.sheet, r, c)) {
                            if let Some(Cell::ArrayFormula { r: (w, h), .. }) =
                                ws.cell(r, c).as_deref()
                            {
                                areas.push(Area {
                                    sheet: area.sheet,
                                    row: r,
                                    column: c,
                                    width: *w,
                                    height: *h,
                                });
                            }
                        }
                    }
                }
            }
        }
        let mut staged = UserModel::from_model(self.model.clipboard_stage(&positions));
        staged.view = self.view.clone();
        staged.pause_evaluation = true;
        let mut scope = crate::display::DisplayEditGuard::new(&mut staged, false);
        scope.target().model.start_display_journal();
        scope.target().paste_from_clipboard_in_display_edit(
            source_sheet,
            source_range,
            clipboard,
            is_cut,
            plan,
        )?;
        let cells = scope.target().model.prepared_display_cells();
        let links = scope.target().model.prepared_dynamic_links();
        let replay = scope.target().model.take_display_journal();
        scope.cancel();
        scope.complete();
        Ok(PreparedClipboard {
            staged,
            cells,
            replay,
            links,
        })
    }

    pub(super) fn commit_clipboard(&mut self, prepared: PreparedClipboard<'a>) {
        let PreparedClipboard {
            mut staged,
            cells,
            replay,
            links,
        } = prepared;
        self.model.retain_prepared_replay(&replay);
        #[cfg(test)]
        {
            self.model.evaluation.display.paste_cells = cells.len();
        }
        self.model.invalidate_calculation();
        for (sheet, row, column, cell) in cells {
            let data = &mut self.model.workbook.worksheets[sheet as usize].sheet_data;
            if let Some(cell) = cell {
                data.set_cell(row, column, cell);
            } else {
                data.remove_cell(row, column);
            }
        }
        for (live, planned) in self
            .model
            .workbook
            .worksheets
            .iter_mut()
            .zip(staged.model.workbook.worksheets)
        {
            live.shared_formulas = planned.shared_formulas;
            live.conditional_formatting = planned.conditional_formatting;
            live.merged_cells = planned.merged_cells;
            live.links = planned.links;
        }
        self.model.workbook.styles = staged.model.workbook.styles;
        self.model.workbook.shared_strings = staged.model.workbook.shared_strings;
        self.model.workbook.defined_names = staged.model.workbook.defined_names;
        self.model.parsed_formulas = staged.model.parsed_formulas;
        self.model.shared_formula_lookup = staged.model.shared_formula_lookup;
        self.model.shared_strings = staged.model.shared_strings;
        self.model.parsed_defined_names = staged.model.parsed_defined_names;
        self.model.parser = staged.model.parser;
        for (p, link) in links {
            if let Some(link) = link {
                if self.model.links.get(&p) != Some(&link) {
                    self.model.insert_dynamic_link(p, link);
                }
            } else {
                self.model.remove_dynamic_link(&p);
            }
        }
        self.view = staged.view;
        if let Some(entry) = staged.history.undo_stack.pop() {
            self.push_diff_list(entry.diffs);
        }
        self.evaluate_if_not_paused();
    }
}

impl<'a> Model<'a> {
    fn clipboard_stage(&self, positions: &HashSet<(u32, i32, i32)>) -> Model<'a> {
        let worksheets = self
            .workbook
            .worksheets
            .iter()
            .map(|ws| Worksheet {
                dimension: ws.dimension.clone(),
                cols: ws.cols.clone(),
                rows: ws.rows.clone(),
                name: ws.name.clone(),
                sheet_data: Default::default(),
                shared_formulas: ws.shared_formulas.clone(),
                sheet_id: ws.sheet_id,
                state: ws.state.clone(),
                color: ws.color.clone(),
                merged_cells: ws.merged_cells.clone(),
                comments: ws.comments.clone(),
                frozen_rows: ws.frozen_rows,
                frozen_columns: ws.frozen_columns,
                show_grid_lines: ws.show_grid_lines,
                conditional_formatting: ws.conditional_formatting.clone(),
                links: ws.links.clone(),
            })
            .collect();
        let workbook = Workbook {
            worksheets,
            shared_strings: self.workbook.shared_strings.clone(),
            defined_names: self.workbook.defined_names.clone(),
            styles: self.workbook.styles.clone(),
            name: self.workbook.name.clone(),
            settings: self.workbook.settings.clone(),
            metadata: self.workbook.metadata.clone(),
            tables: self.workbook.tables.clone(),
            theme: self.workbook.theme.clone(),
        };
        let mut model = Model {
            workbook,
            parsed_formulas: self.parsed_formulas.clone(),
            parsed_defined_names: self.parsed_defined_names.clone(),
            shared_strings: self.shared_strings.clone(),
            parser: self.parser.clone(),
            locale: self.locale,
            language: self.language,
            tz: self.tz.clone(),
            variable_stack: Default::default(),
            last_variable_id: 0,
            lambdas: Default::default(),
            last_lambda_id: 0,
            evaluation: Default::default(),
            dependency_index: None,
            cf_cache: Default::default(),
            links: self
                .links
                .iter()
                .filter(|(p, _)| positions.contains(p))
                .map(|(p, l)| (*p, l.clone()))
                .collect(),
            shared_formula_lookup: self.shared_formula_lookup.clone(),
        };
        for &(s, r, c) in positions {
            if let Some(cell) = self.workbook.worksheets[s as usize].cell(r, c) {
                model.workbook.worksheets[s as usize]
                    .sheet_data
                    .set_cell(r, c, cell.into_owned());
            }
        }
        model
    }
}
