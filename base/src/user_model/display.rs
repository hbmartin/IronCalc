use super::history::{Diff, DiffType, QueueDiffs};
use crate::{types::WorkbookSettings, UserModel};

impl UserModel<'_> {
    pub(super) fn with_user_display_edit<T>(
        &mut self,
        record: bool,
        edit: impl FnOnce(&mut Self) -> Result<T, String>,
    ) -> Result<T, String> {
        let outermost = self.model.evaluation.display.edit_depth == 0;
        let journaled = outermost && record;
        let entries = self.history.undo_stack.len();
        crate::display::DisplayEditGuard::run(
            self,
            |model| {
                if journaled {
                    model.model.start_display_journal();
                }
                edit(model)
            },
            |_| {},
            |model| {
                if journaled {
                    let difference = model.model.take_display_journal();
                    if model.history.undo_stack.len() > entries {
                        if let Some(entry) = model.history.undo_stack.last_mut() {
                            entry.display = std::sync::Arc::new(difference);
                        }
                    }
                }
            },
        )
    }

    fn preflight_context_transition(
        &self,
        settings: &mut WorkbookSettings,
        diffs: &[Diff],
        undo: bool,
    ) -> Result<(), String> {
        let visit = |diff: &Diff, settings: &mut WorkbookSettings| -> Result<(), String> {
            match diff {
                Diff::SetIterativeCalculation {
                    old_value,
                    new_value,
                } => {
                    settings.iterative_calculation =
                        if undo { old_value } else { new_value }.clone()
                }
                Diff::SetTimezone {
                    old_value,
                    new_value,
                }
                | Diff::SetLocale {
                    old_value,
                    new_value,
                } => {
                    let new = if undo { old_value } else { new_value };
                    let current = if matches!(diff, Diff::SetTimezone { .. }) {
                        &settings.tz
                    } else {
                        &settings.locale
                    };
                    if current == new {
                        return Ok(());
                    }
                    if self.pause_evaluation {
                        return Err(
                            "Cannot change calculation context while evaluation is paused".into(),
                        );
                    }
                    if settings.iterative_calculation.enabled
                        && settings.iterative_calculation.validate().is_err()
                    {
                        return Err("Cannot change calculation context with invalid iterative calculation settings".into());
                    }
                    if matches!(diff, Diff::SetTimezone { .. }) {
                        crate::tz::Tz::parse(new)?;
                        settings.tz = new.clone();
                    } else {
                        crate::locale::get_locale(new)?;
                        settings.locale = new.clone();
                    }
                }
                _ => {}
            }
            Ok(())
        };
        if undo {
            for diff in diffs.iter().rev() {
                visit(diff, settings)?;
            }
        } else {
            for diff in diffs {
                visit(diff, settings)?;
            }
        }
        Ok(())
    }

    pub(super) fn preflight_context_diffs(&self, diffs: &[Diff], undo: bool) -> Result<(), String> {
        self.preflight_context_transition(&mut self.model.workbook.settings.clone(), diffs, undo)
    }

    pub(super) fn preflight_context_queue(&self, queue: &[QueueDiffs]) -> Result<(), String> {
        let mut settings = self.model.workbook.settings.clone();
        for item in queue {
            self.preflight_context_transition(
                &mut settings,
                &item.list,
                matches!(item.r#type, DiffType::Undo),
            )?;
        }
        Ok(())
    }
}

impl UserModel<'_> {
    /// Footprints describe explicit edits, never dependent calculation writes.
    pub(super) fn apply_external_display_diff(
        &mut self,
        diff: &Diff,
        undo: bool,
        conflicts: &mut crate::display::DisplayConflicts,
    ) -> Result<(), String> {
        use crate::expressions::types::Area;
        let mut area = None;
        let mut structural_sheet = None;
        let mut structural = false;
        let mut added_sheet = None;
        match diff {
            Diff::SetCellValue {
                sheet, row, column, ..
            } => {
                area = Some(Area {
                    sheet: *sheet,
                    row: *row,
                    column: *column,
                    width: 1,
                    height: 1,
                })
            }
            Diff::SetArrayValue {
                sheet,
                row,
                column,
                width,
                height,
                ..
            }
            | Diff::RangeClearContents {
                sheet,
                row,
                column,
                width,
                height,
                ..
            }
            | Diff::RangeClearAll {
                sheet,
                row,
                column,
                width,
                height,
                ..
            } => {
                area = Some(Area {
                    sheet: *sheet,
                    row: *row,
                    column: *column,
                    width: *width,
                    height: *height,
                })
            }
            Diff::SetMergedCells {
                sheet,
                old_value,
                new_value,
            } => {
                let current = self.model.get_merged_cells(*sheet)?;
                let desired = if undo { old_value } else { new_value };
                for m in current
                    .iter()
                    .filter(|m| !desired.contains(m))
                    .chain(desired.iter().filter(|m| !current.contains(m)))
                {
                    conflicts.area(
                        &self.model,
                        &Area {
                            sheet: *sheet,
                            row: m.row,
                            column: m.column,
                            width: m.width,
                            height: m.height,
                        },
                    );
                }
            }
            Diff::InsertRows { sheet, .. }
            | Diff::DeleteRows { sheet, .. }
            | Diff::InsertColumns { sheet, .. }
            | Diff::DeleteColumns { sheet, .. }
            | Diff::MoveRows { sheet, .. }
            | Diff::MoveColumns { sheet, .. } => {
                structural = true;
                structural_sheet = Some(*sheet);
            }
            Diff::RenameSheet { index, .. } => {
                structural = true;
                structural_sheet = Some(*index);
            }
            Diff::MoveSheet {
                sheet_index,
                new_index,
            } => {
                structural = true;
                structural_sheet = Some(if undo { *new_index } else { *sheet_index });
            }
            Diff::DeleteSheet { old_data, .. } => {
                structural = true;
                conflicts.sheets.insert(old_data.sheet_id);
            }
            Diff::NewSheet { index, .. } => {
                structural = true;
                if undo {
                    structural_sheet = Some(*index)
                } else {
                    added_sheet = Some(*index)
                }
            }
            Diff::DuplicateSheet { new_index, .. } => {
                structural = true;
                if undo {
                    structural_sheet = Some(*new_index)
                } else {
                    added_sheet = Some(*new_index)
                }
            }
            Diff::SetLocale {
                old_value,
                new_value,
            } => {
                conflicts.all |=
                    self.model.workbook.settings.locale != *if undo { old_value } else { new_value }
            }
            Diff::SetTimezone {
                old_value,
                new_value,
            } => {
                conflicts.all |=
                    self.model.workbook.settings.tz != *if undo { old_value } else { new_value }
            }
            Diff::SetIterativeCalculation {
                old_value,
                new_value,
            } => {
                conflicts.all |= self.model.workbook.settings.iterative_calculation
                    != *if undo { old_value } else { new_value }
            }
            Diff::CreateDefinedName { .. }
            | Diff::DeleteDefinedName { .. }
            | Diff::UpdateDefinedName { .. } => conflicts.all = true,
            Diff::CellClearFormatting { .. }
            | Diff::SetCellStyle { .. }
            | Diff::ApplyNamedStyle { .. }
            | Diff::SetColumnWidth { .. }
            | Diff::SetColumnHidden { .. }
            | Diff::SetRowHidden { .. }
            | Diff::SetRowHeight { .. }
            | Diff::SetColumnStyle { .. }
            | Diff::SetRowStyle { .. }
            | Diff::DeleteColumnStyle { .. }
            | Diff::DeleteRowStyle { .. }
            | Diff::SetFrozenRowsCount { .. }
            | Diff::SetFrozenColumnsCount { .. }
            | Diff::SetSheetColor { .. }
            | Diff::SetSheetState { .. }
            | Diff::SetShowGridLines { .. }
            | Diff::SetTheme { .. }
            | Diff::SetWorkbookName { .. }
            | Diff::CreateNamedStyle { .. }
            | Diff::DeleteNamedStyle { .. }
            | Diff::UpdateNamedStyle { .. }
            | Diff::AddConditionalFormatting { .. }
            | Diff::DeleteConditionalFormatting { .. }
            | Diff::UpdateConditionalFormatting { .. }
            | Diff::SwapConditionalFormattingPriority { .. }
            | Diff::SetCellLink { .. } => {}
        }
        if let Some(ref area) = area {
            conflicts.area(&self.model, area);
        }
        if let Some(sheet) = structural_sheet {
            if let Ok(ws) = self.model.workbook.worksheet(sheet) {
                conflicts.sheets.insert(ws.sheet_id);
            }
        }
        let formulas: Vec<_> = if structural {
            self.model
                .workbook
                .worksheets
                .iter()
                .map(|ws| (ws.sheet_id, ws.shared_formulas.clone()))
                .collect()
        } else {
            Vec::new()
        };
        let result = if undo {
            self.apply_undo_diff_list(std::slice::from_ref(diff))
        } else {
            self.apply_diff_list(std::slice::from_ref(diff))
        };
        if let Some(ref area) = area {
            conflicts.area(&self.model, area);
        }
        if let Some(sheet) = added_sheet {
            if let Ok(ws) = self.model.workbook.worksheet(sheet) {
                conflicts.sheets.insert(ws.sheet_id);
            }
        }
        for (id, before) in formulas {
            if self
                .model
                .workbook
                .worksheets
                .iter()
                .find(|ws| ws.sheet_id == id)
                .is_none_or(|ws| ws.shared_formulas != before)
            {
                conflicts.sheets.insert(id);
            }
        }
        result
    }

    pub(super) fn discard_external_display_conflicts(
        &mut self,
        conflicts: &crate::display::DisplayConflicts,
    ) {
        if conflicts.is_empty() {
            return;
        }
        #[cfg(test)]
        {
            self.model.evaluation.display.conflict_batches += 1;
        }
        for entry in self
            .history
            .undo_stack
            .iter_mut()
            .chain(&mut self.history.redo_stack)
        {
            #[cfg(test)]
            {
                self.model.evaluation.display.history_scans += 1;
            }
            if entry.display.intersects(conflicts) {
                std::sync::Arc::make_mut(&mut entry.display).discard_conflicts(conflicts);
            }
        }
    }
}

impl<'a> crate::display::DisplayEditTarget<'a> for crate::UserModel<'a> {
    fn calculate_after_edit(&self) -> bool {
        !self.pause_evaluation
    }
    fn edit_model(&mut self) -> &mut crate::Model<'a> {
        &mut self.model
    }
}
