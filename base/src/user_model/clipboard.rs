#![deny(missing_docs)]

use std::borrow::Cow;
use std::{
    collections::{HashMap, HashSet},
    io::Cursor,
};

use csv::{ReaderBuilder, WriterBuilder};
use serde::{Deserialize, Serialize};

use crate::{
    cf_types::{CfRule, ConditionalFormatting},
    constants::{LAST_COLUMN, LAST_ROW},
    expressions::types::{Area, CellReferenceIndex},
    model::CellStructure,
    types::{ArrayKind, Cell, Link, MergedCell, Style},
    UserModel,
};

use crate::user_model::history::Diff;

const PARTIAL_MERGE_PASTE_ERROR: &str =
    "Cannot paste: a merged cell partially overlaps the paste area";

/// Data for the clipboard
pub type ClipboardData = HashMap<i32, HashMap<i32, ClipboardCell>>;

pub type ClipboardTuple = (i32, i32, i32, i32);

#[derive(Serialize, Deserialize)]
pub struct ClipboardCell {
    text: String,
    is_spill: bool,
    style: Style,
    // the link attached to the cell, if any (`default` keeps older clipboard
    // payloads without the field deserializable)
    #[serde(default)]
    link: Option<Link>,
}

#[derive(Serialize, Deserialize)]
pub struct Clipboard {
    pub(crate) csv: String,
    pub(crate) data: ClipboardData,
    pub(crate) sheet: u32,
    pub(crate) range: (i32, i32, i32, i32),
}

type FormulaUpdate = (u32, i32, i32, String);
type NameUpdate = (String, Option<u32>, String, String);
type FormattingUpdate = (u32, usize, String, CfRule);

/// Only validated input reaches the mutating phase. No formulas run in planning.
struct ClipboardPastePlan {
    values: HashMap<(i32, i32), String>,
    formula_updates: Vec<FormulaUpdate>,
    name_updates: Vec<NameUpdate>,
    formatting_updates: Vec<FormattingUpdate>,
}

impl<'a> UserModel<'a> {
    // Applies the merge containment rule to a paste into `target_area`
    // (whose top-left cell is the paste anchor). The paste footprint is the
    // target rectangle plus `pasted_merges`, the translated merges of the
    // copied area it will recreate — they can stick out of the target
    // because the copied range is clamped to the sheet dimension while a
    // merge always travels whole.
    //
    // * a merged cell fully contained in the footprint is removed — the
    //   paste replaces it. The removal diff is pushed before the cell diffs
    //   so a forward replay writes on unmerged cells; the merges the paste
    //   creates get their own diff afterwards, so an undo unmerges them
    //   first.
    // * a single-cell paste into the anchor of a merged cell keeps the
    //   merge (only the anchor is written);
    // * any other overlap rejects the paste, before anything changed.
    fn plan_merges_replaced_by_paste(
        &self,
        target_area: &Area,
        pasted_merges: &[MergedCell],
    ) -> Result<Option<Vec<MergedCell>>, String> {
        let sheet = target_area.sheet;
        let old_merged_cells = self.model.get_merged_cells(sheet)?.to_vec();
        let single_cell_target = target_area.width == 1 && target_area.height == 1;
        let cell_in_footprint = |row: i32, column: i32| -> bool {
            (row >= target_area.row
                && row < target_area.row + target_area.height
                && column >= target_area.column
                && column < target_area.column + target_area.width)
                || pasted_merges.iter().any(|p| {
                    row >= p.row
                        && row < p.row + p.height
                        && column >= p.column
                        && column < p.column + p.width
                })
        };
        let mut merged_cells_after_removal = Vec::new();
        let mut removed_any = false;
        for m in &old_merged_cells {
            let intersects_footprint = m.intersects(
                target_area.row,
                target_area.column,
                target_area.width,
                target_area.height,
            ) || pasted_merges
                .iter()
                .any(|p| m.intersects(p.row, p.column, p.width, p.height));
            if !intersects_footprint {
                merged_cells_after_removal.push(*m);
                continue;
            }
            let mut contained = true;
            'cells: for row in m.row..m.row + m.height {
                for column in m.column..m.column + m.width {
                    if !cell_in_footprint(row, column) {
                        contained = false;
                        break 'cells;
                    }
                }
            }
            if contained {
                // replaced by the paste
                removed_any = true;
            } else if single_cell_target
                && (m.row, m.column) == (target_area.row, target_area.column)
            {
                // pasting a single cell into a merged cell writes its anchor
                merged_cells_after_removal.push(*m);
            } else {
                return Err(PARTIAL_MERGE_PASTE_ERROR.to_string());
            }
        }
        Ok(removed_any.then_some(merged_cells_after_removal))
    }

    fn remove_merges_replaced_by_paste(
        &mut self,
        target_area: &Area,
        pasted_merges: &[MergedCell],
        diff_list: &mut Vec<Diff>,
    ) -> Result<(), String> {
        if let Some(new_value) = self.plan_merges_replaced_by_paste(target_area, pasted_merges)? {
            let ws = self.model.workbook.worksheet_mut(target_area.sheet)?;
            let old_value = std::mem::replace(&mut ws.merged_cells, new_value.clone());
            diff_list.push(Diff::SetMergedCells {
                sheet: target_area.sheet,
                old_value,
                new_value,
            });
        }
        Ok(())
    }

    /// Returns a copy of the selected area
    pub fn copy_to_clipboard(&self) -> Result<Clipboard, String> {
        let selected_area = self.get_selected_view();
        let sheet = selected_area.sheet;
        let mut wtr = WriterBuilder::new().delimiter(b'\t').from_writer(vec![]);

        let mut data = HashMap::new();
        let [row_start, column_start, row_end, column_end] = selected_area.range;
        let dimension = self.model.workbook.worksheet(sheet)?.dimension();
        let row_end = row_end.min(dimension.max_row).max(row_start);
        let column_end = column_end.min(dimension.max_column).max(column_start);
        for row in row_start..=row_end {
            let mut data_row = HashMap::new();
            let mut text_row = Vec::new();
            for column in column_start..=column_end {
                let text = self.get_formatted_cell_value(sheet, row, column)?;
                let content = self.get_cell_content(sheet, row, column)?;
                let style = self.model.get_style_for_cell(sheet, row, column)?;
                let is_spill = matches!(
                    self.model.get_cell_structure(sheet, row, column)?,
                    CellStructure::SpillArray { .. } | CellStructure::SpillDynamic { .. }
                );
                let link = self.model.get_cell_link(sheet, row, column)?;
                data_row.insert(
                    column,
                    ClipboardCell {
                        text: content,
                        is_spill,
                        style,
                        link,
                    },
                );
                text_row.push(text);
            }
            wtr.write_record(text_row)
                .map_err(|e| format!("Error while processing csv: {e}"))?;
            data.insert(row, data_row);
        }

        let csv = String::from_utf8(
            wtr.into_inner()
                .map_err(|e| format!("Processing error: '{e}'"))?,
        )
        .map_err(|e| format!("Error converting from utf8: '{e}'"))?;

        Ok(Clipboard {
            csv: csv.trim().to_string(),
            data,
            sheet,
            range: (row_start, column_start, row_end, column_end),
        })
    }

    /// Paste text that we copied
    pub fn paste_from_clipboard(
        &mut self,
        source_sheet: u32,
        source_range: ClipboardTuple,
        clipboard: &ClipboardData,
        is_cut: bool,
    ) -> Result<(), String> {
        let plan = self.plan_clipboard_paste(source_sheet, source_range, clipboard, is_cut)?;
        self.with_user_display_edit(true, |this| {
            this.paste_from_clipboard_in_display_edit(
                source_sheet,
                source_range,
                clipboard,
                is_cut,
                plan,
            )
        })
    }

    fn plan_clipboard_paste(
        &mut self,
        source_sheet: u32,
        source_range: ClipboardTuple,
        clipboard: &ClipboardData,
        is_cut: bool,
    ) -> Result<ClipboardPastePlan, String> {
        let (r1, c1, r2, c2) = source_range;
        let view = self.get_selected_view();
        let source = Area {
            sheet: source_sheet,
            row: r1,
            column: c1,
            width: c2.saturating_sub(c1).saturating_add(1),
            height: r2.saturating_sub(r1).saturating_add(1),
        };
        let target = Area {
            sheet: view.sheet,
            row: view.row,
            column: view.column,
            width: source.width,
            height: source.height,
        };
        for area in [&source, &target] {
            self.model.workbook.worksheet(area.sheet)?;
            if area.width <= 0
                || area.height <= 0
                || area.row < 1
                || area.column < 1
                || i64::from(area.row) + i64::from(area.height) - 1 > i64::from(LAST_ROW)
                || i64::from(area.column) + i64::from(area.width) - 1 > i64::from(LAST_COLUMN)
            {
                return Err("Clipboard range out of boundaries".into());
            }
        }
        if !self.model.can_clear_range(&target)?
            || (is_cut && !self.model.can_clear_range(&source)?)
        {
            return Err("Cannot clear the range because it contains array formulas".into());
        }
        let merges: Vec<_> = self
            .model
            .get_merged_cells(source_sheet)?
            .iter()
            .filter(|m| m.intersects(r1, c1, source.width, source.height))
            .map(|m| MergedCell {
                row: m.row + target.row - r1,
                column: m.column + target.column - c1,
                width: m.width,
                height: m.height,
            })
            .collect();
        self.plan_merges_replaced_by_paste(&target, &merges)?;
        let mut values = HashMap::new();
        for (&row, data) in clipboard {
            for (&column, cell) in data {
                if row < r1 || row > r2 || column < c1 || column > c2 {
                    return Err("Source is outside the area".into());
                }
                let target_row = target.row + row - r1;
                let target_column = target.column + column - c1;
                self.model
                    .get_style_for_cell(target.sheet, target_row, target_column)?;
                self.model.get_style_for_cell(source_sheet, row, column)?;
                if cell.is_spill {
                    continue;
                }
                if !cell.text.starts_with('\'')
                    && self.model.formula_without_prefix(&cell.text).is_none()
                {
                    let mut currencies = vec!["$", "€"];
                    let currency = &self.model.locale.currency.symbol;
                    if !currencies.contains(&currency.as_str()) {
                        currencies.push(currency);
                    }
                    if let Ok((number, _)) = crate::formatter::format::parse_formatted_number(
                        &cell.text,
                        &currencies,
                        self.model.locale,
                    ) {
                        if !number.is_finite() {
                            return Err(format!("A cell cannot hold the number {number}"));
                        }
                    }
                }
                let from = CellReferenceIndex {
                    sheet: source_sheet,
                    row,
                    column,
                };
                let to = CellReferenceIndex {
                    sheet: target.sheet,
                    row: target.row + row - r1,
                    column: target.column + column - c1,
                };
                let value = if is_cut {
                    self.model
                        .move_cell_value_to_area(&cell.text, &from, &to, &source)?
                } else {
                    self.model.extend_copied_value(&cell.text, &from, &to)?
                };
                values.insert((row, column), value);
            }
        }
        let (formula_updates, name_updates, formatting_updates) = if is_cut {
            let updates = self
                .model
                .get_external_formula_updates_for_cut(&source, target.row, target.column)?
                .into_iter()
                .filter(|&(s, r, c, _)| {
                    !(s == target.sheet
                        && r >= target.row
                        && r < target.row + target.height
                        && c >= target.column
                        && c < target.column + target.width)
                })
                .collect();
            let names =
                self.model
                    .get_defined_name_updates_for_cut(&source, target.row, target.column);
            for (name, _, _, formula) in &names {
                if !crate::expressions::utils::is_valid_identifier(name) {
                    return Err("Name: Invalid defined name".into());
                }
                let context = self.model.defined_name_context();
                self.model.user_formula_to_internal(formula, &context)?;
            }
            let formatting = self.model.get_conditional_formatting_updates_for_cut(
                &source,
                target.row,
                target.column,
            )?;
            (updates, names, formatting)
        } else {
            (Vec::new(), Vec::new(), Vec::new())
        };
        Ok(ClipboardPastePlan {
            values,
            formula_updates,
            name_updates,
            formatting_updates,
        })
    }

    fn paste_from_clipboard_in_display_edit(
        &mut self,
        source_sheet: u32,
        source_range: ClipboardTuple,
        clipboard: &ClipboardData,
        is_cut: bool,
        mut plan: ClipboardPastePlan,
    ) -> Result<(), String> {
        let retained_cut = is_cut.then(|| self.model.display_snapshot());
        let source_id = self.model.workbook.worksheet(source_sheet)?.sheet_id;
        if is_cut {
            // Moving cells also rewrites references outside the pasted area.
            // Invalidate before any mutation, including while evaluation is paused.
            self.model.invalidate_calculation();
        }
        let mut diff_list = Vec::new();
        let view = self.get_selected_view();
        let (source_first_row, source_first_column, source_last_row, source_last_column) =
            source_range;
        let sheet = view.sheet;
        // Paste is anchored at the selected cell (the range is normalized, so
        // its start corner is not necessarily where the selection began)
        let (selected_row, selected_column) = (view.row, view.column);
        let mut max_row = selected_row;
        let mut max_column = selected_column;
        let target_area = &Area {
            sheet,
            row: selected_row,
            column: selected_column,
            width: source_last_column - source_first_column + 1,
            height: source_last_row - source_first_row + 1,
        };

        // The merges of the copied area, captured before the containment
        // step below can remove target merges: when the paste overlaps its
        // own source, a source merge can also be a replaced target merge and
        // must still be recreated.
        let source_merges: Vec<MergedCell> = self
            .model
            .get_merged_cells(source_sheet)?
            .iter()
            .filter(|m| {
                m.intersects(
                    source_first_row,
                    source_first_column,
                    source_last_column - source_first_column + 1,
                    source_last_row - source_first_row + 1,
                )
            })
            .copied()
            .collect();

        // Merged cells in the target are replaced by the paste (or reject it)
        let pasted_merges: Vec<MergedCell> = source_merges
            .iter()
            .map(|m| MergedCell {
                row: m.row + selected_row - source_first_row,
                column: m.column + selected_column - source_first_column,
                width: m.width,
                height: m.height,
            })
            .collect();
        self.remove_merges_replaced_by_paste(target_area, &pasted_merges, &mut diff_list)?;

        let mut seen_cells = HashSet::new();
        // Compute all changes
        let mut changes = Vec::new();
        for (source_row, data_row) in clipboard {
            let delta_row = source_row - source_first_row;
            let target_row = selected_row + delta_row;
            max_row = max_row.max(target_row);
            for (source_column, value) in data_row {
                let delta_column = source_column - source_first_column;
                let target_column = selected_column + delta_column;
                max_column = max_column.max(target_column);

                if value.is_spill {
                    // Spill cells carry no formula/value, but their style should still be copied.
                    let old_style =
                        self.model
                            .get_cell_style_or_none(sheet, target_row, target_column)?;
                    changes.push((
                        target_row,
                        target_column,
                        None,
                        old_style,
                        None,
                        value.style.clone(),
                    ));
                    seen_cells.insert((target_row, target_column));
                    continue;
                }

                // We are copying the value in
                // (source_row, source_column) to (target_row , target_column)
                // References in formulas are displaced

                // Prepared before the edit scope, including all formula rewrites.
                let new_value = plan
                    .values
                    .remove(&(*source_row, *source_column))
                    .ok_or_else(|| "Missing prepared clipboard cell".to_string())?;

                let old_value = self
                    .model
                    .workbook
                    .worksheet(sheet)?
                    .cell(target_row, target_column)
                    .map(Cow::into_owned);

                let old_style =
                    self.model
                        .get_cell_style_or_none(sheet, target_row, target_column)?;
                changes.push((
                    target_row,
                    target_column,
                    old_value.clone(),
                    old_style.clone(),
                    Some(new_value.clone()),
                    value.style.clone(),
                ));
                seen_cells.insert((target_row, target_column));
            }
        }
        // Clearing the target area also removes its links: capture them for undo
        diff_list.extend(self.range_link_diffs(target_area)?);
        // clear the whole area (this resets array formulas)
        self.model.range_clear_contents(target_area)?;
        // set the new values and styles
        for (target_row, target_column, old_value, old_style, new_value, style) in changes {
            if let Some(ref v) = new_value {
                self.model
                    .set_user_input(sheet, target_row, target_column, v.clone())?;
                diff_list.push(Diff::SetCellValue {
                    sheet,
                    row: target_row,
                    column: target_column,
                    new_value: v.clone(),
                    old_value: Box::new(old_value),
                });
            }
            self.model
                .set_cell_style(sheet, target_row, target_column, &style)?;

            diff_list.push(Diff::SetCellStyle {
                sheet,
                row: target_row,
                column: target_column,
                old_value: Box::new(old_style),
                new_value: Box::new(style),
            });
        }
        // Paste the links of the copied cells. This runs after the values are
        // set so that it also overrides any link auto-created by an URL value.
        for (source_row, data_row) in clipboard {
            let target_row = selected_row + (source_row - source_first_row);
            for (source_column, value) in data_row {
                let target_column = selected_column + (source_column - source_first_column);
                let old_link = self.model.get_cell_link(sheet, target_row, target_column)?;
                if old_link == value.link {
                    continue;
                }
                match &value.link {
                    Some(link) => {
                        self.model
                            .set_cell_link(sheet, target_row, target_column, link.clone())?
                    }
                    None => self
                        .model
                        .delete_cell_link(sheet, target_row, target_column)?,
                }
                diff_list.push(Diff::SetCellLink {
                    sheet,
                    row: target_row,
                    column: target_column,
                    old_value: Box::new(old_link),
                    new_value: Box::new(value.link.clone()),
                });
            }
        }
        if is_cut {
            // A cut moves the merged cells of the source area to the target:
            // they are removed from the source and recreated, translated by
            // the paste offset. A merge is matched by intersection: it can
            // stick out of the cut range when its covered cells are empty (the
            // copied range is clamped to the sheet dimension), and it moves
            // whole.
            let old_source_merged_cells = self.model.get_merged_cells(source_sheet)?.to_vec();
            let old_target_merged_cells = self.model.get_merged_cells(sheet)?.to_vec();
            self.model.unmerge_cells(&Area {
                sheet: source_sheet,
                row: source_first_row,
                column: source_first_column,
                width: source_last_column - source_first_column + 1,
                height: source_last_row - source_first_row + 1,
            })?;
            for m in source_merges {
                let merge_area = Area {
                    sheet,
                    row: m.row + selected_row - source_first_row,
                    column: m.column + selected_column - source_first_column,
                    width: m.width,
                    height: m.height,
                };
                // Capture the content and links the merge will clear: a merge
                // sticking out of the pasted block can swallow cells that were
                // not part of the paste
                let mut merge_diffs = self.covered_cells_clear_diffs(&merge_area)?;
                // A translated merge can conflict outside the checked target
                // area (another merge, an array formula) or fall off the
                // sheet: those are dropped, the rest of the cut still lands
                // merge_cells_keep_styles: the pasted cells already carry the
                // merged style pattern of the source; stamping the anchor's
                // style would drop the perimeter borders of non-anchor cells
                if self.model.merge_cells_keep_styles(&merge_area).is_ok() {
                    diff_list.append(&mut merge_diffs);
                }
            }
            // One snapshot per touched sheet (source and target coincide on a
            // same-sheet cut)
            let mut snapshot_sheets = vec![source_sheet];
            if sheet != source_sheet {
                snapshot_sheets.push(sheet);
            }
            for (snapshot_sheet, old_value) in snapshot_sheets
                .into_iter()
                .zip([old_source_merged_cells, old_target_merged_cells])
            {
                let new_value = self.model.get_merged_cells(snapshot_sheet)?.to_vec();
                if old_value != new_value {
                    diff_list.push(Diff::SetMergedCells {
                        sheet: snapshot_sheet,
                        old_value,
                        new_value,
                    });
                }
            }
            self.model.collapse_partially_cleared_spills(&Area {
                sheet: source_sheet,
                row: source_first_row,
                column: source_first_column,
                width: source_last_column - source_first_column + 1,
                height: source_last_row - source_first_row + 1,
            })?;
            for row in source_first_row..=source_last_row {
                for column in source_first_column..=source_last_column {
                    if (source_sheet == sheet) && seen_cells.contains(&(row, column)) {
                        continue;
                    }
                    let old_value = self
                        .model
                        .workbook
                        .worksheet(source_sheet)?
                        .cell(row, column)
                        .map(Cow::into_owned);

                    diff_list.push(Diff::RangeClearContents {
                        sheet: source_sheet,
                        row,
                        column,
                        width: 1,
                        height: 1,
                        old_value: vec![vec![old_value.clone()]],
                    });

                    // a cut also moves the link away from the source cell
                    let old_link = self.model.get_cell_link(source_sheet, row, column)?;
                    if let Some(old_link) = old_link {
                        self.model.delete_cell_link(source_sheet, row, column)?;
                        diff_list.push(Diff::SetCellLink {
                            sheet: source_sheet,
                            row,
                            column,
                            old_value: Box::new(Some(old_link)),
                            new_value: Box::new(None),
                        });
                    }

                    // If the source is a dynamic formula anchor, range_clear_contents
                    // would erase its entire spill — including cells that were just
                    // written to by this paste.  Clear the anchor and its spill cells
                    // individually instead, skipping any paste-target cells.
                    let spill_dims = match &old_value {
                        Some(Cell::ArrayFormula {
                            kind: ArrayKind::Dynamic,
                            r,
                            ..
                        }) => Some(*r),
                        _ => None,
                    };
                    if let Some((spill_w, spill_h)) = spill_dims {
                        let ws = self.model.workbook.worksheet_mut(source_sheet)?;
                        for sr in row..row + spill_h {
                            for sc in column..column + spill_w {
                                if (source_sheet == sheet) && seen_cells.contains(&(sr, sc)) {
                                    continue;
                                }
                                let _ = ws.cell_clear_contents(sr, sc);
                            }
                        }
                    } else {
                        self.model
                            .workbook
                            .worksheet_mut(source_sheet)?
                            .cell_clear_contents(row, column)?;
                    }
                    let old_style = self
                        .model
                        .get_cell_style_or_none(source_sheet, row, column)?;
                    let default_style = Style::default();
                    self.model
                        .set_cell_style(source_sheet, row, column, &default_style)?;
                    diff_list.push(Diff::SetCellStyle {
                        sheet: source_sheet,
                        row,
                        column,
                        old_value: Box::new(old_style),
                        new_value: Box::new(default_style),
                    });
                }
            }
            // Update external formulas that reference cells in the moved area.
            // source_sheet is used here (not `sheet`) so cross-sheet paste works.
            for (ext_sheet, ext_row, ext_col, new_formula) in plan.formula_updates {
                let old_cell = self
                    .model
                    .workbook
                    .worksheet(ext_sheet)?
                    .cell(ext_row, ext_col)
                    .map(Cow::into_owned);
                self.model
                    .set_user_input(ext_sheet, ext_row, ext_col, new_formula.clone())?;
                diff_list.push(Diff::SetCellValue {
                    sheet: ext_sheet,
                    row: ext_row,
                    column: ext_col,
                    new_value: new_formula,
                    old_value: Box::new(old_cell),
                });
            }
            // Update defined names whose references land inside the moved area.
            for (dn_name, dn_scope, old_formula, new_formula) in plan.name_updates {
                diff_list.push(Diff::UpdateDefinedName {
                    name: dn_name.clone(),
                    scope: dn_scope,
                    old_formula: old_formula.clone(),
                    new_name: dn_name.clone(),
                    new_scope: dn_scope,
                    new_formula: new_formula.clone(),
                });
                self.model.update_defined_name(
                    &dn_name,
                    dn_scope,
                    &dn_name,
                    dn_scope,
                    &new_formula,
                )?;
            }
            // Update conditional formatting ranges and formula references.
            for (cf_sheet, cf_idx, new_range, new_rule) in plan.formatting_updates {
                let old_cf = self
                    .model
                    .workbook
                    .worksheet(cf_sheet)?
                    .conditional_formatting
                    .get(cf_idx)
                    .ok_or_else(|| format!("CF index {cf_idx} not found"))?
                    .clone();
                {
                    let ws = self.model.workbook.worksheet_mut(cf_sheet)?;
                    ws.conditional_formatting[cf_idx].range = new_range.clone();
                    ws.conditional_formatting[cf_idx].cf_rule = new_rule.clone();
                }
                diff_list.push(Diff::UpdateConditionalFormatting {
                    sheet: cf_sheet,
                    index: cf_idx as u32,
                    old_range: old_cf.range,
                    old_rule: Box::new(old_cf.cf_rule),
                    old_priority: old_cf.priority,
                    new_range,
                    new_rule: Box::new(new_rule),
                });
            }
        } else {
            // Copy-paste recreates the merged cells of the copied area at the
            // target. A merge is matched by intersection: it can stick out of
            // the copied range when its covered cells are empty (the copied
            // range is clamped to the sheet dimension), and is recreated whole.
            if !source_merges.is_empty() {
                let old_merged_cells = self.model.get_merged_cells(sheet)?.to_vec();
                for m in source_merges {
                    let merge_area = Area {
                        sheet,
                        row: m.row + selected_row - source_first_row,
                        column: m.column + selected_column - source_first_column,
                        width: m.width,
                        height: m.height,
                    };
                    // Capture the content and links the merge will clear: a
                    // merge sticking out of the pasted block can swallow cells
                    // that were not part of the paste
                    let mut merge_diffs = self.covered_cells_clear_diffs(&merge_area)?;
                    // A translated merge can conflict outside the checked
                    // target area (another merge, an array formula) or fall
                    // off the sheet: those are skipped, the rest still paste
                    // merge_cells_keep_styles: the pasted cells already carry
                    // the merged style pattern of the source; stamping the
                    // anchor's style would drop the perimeter borders of
                    // non-anchor cells
                    if self.model.merge_cells_keep_styles(&merge_area).is_ok() {
                        diff_list.append(&mut merge_diffs);
                    }
                }
                let new_merged_cells = self.model.get_merged_cells(sheet)?.to_vec();
                if old_merged_cells != new_merged_cells {
                    diff_list.push(Diff::SetMergedCells {
                        sheet,
                        old_value: old_merged_cells,
                        new_value: new_merged_cells,
                    });
                }
            }

            // Copy-paste: duplicate CF rules from the source area to the target.
            let cf_copies = self.model.get_cf_rules_to_copy(
                source_sheet,
                source_first_row,
                source_first_column,
                source_last_row,
                source_last_column,
                selected_row,
                selected_column,
            );
            for (new_range, new_rule) in cf_copies {
                let priority = self
                    .model
                    .workbook
                    .worksheet(sheet)?
                    .conditional_formatting
                    .iter()
                    .map(|cf| cf.priority)
                    .max()
                    .map(|m| m + 1)
                    .unwrap_or(1);
                self.model
                    .workbook
                    .worksheet_mut(sheet)?
                    .conditional_formatting
                    .push(ConditionalFormatting {
                        range: new_range.clone(),
                        cf_rule: new_rule.clone(),
                        priority,
                    });
                diff_list.push(Diff::AddConditionalFormatting {
                    sheet,
                    range: new_range,
                    rule: Box::new(new_rule),
                    priority,
                });
            }
        }
        if let Some(before) = retained_cut {
            let target_id = self.model.workbook.worksheet(sheet)?.sheet_id;
            self.model.restore_mapped_display(before, |p| {
                if p.0 == source_id
                    && p.1 >= source_first_row
                    && p.1 <= source_last_row
                    && p.2 >= source_first_column
                    && p.2 <= source_last_column
                {
                    Some((
                        target_id,
                        p.1 + selected_row - source_first_row,
                        p.2 + selected_column - source_first_column,
                    ))
                } else if p.0 == target_id
                    && p.1 >= selected_row
                    && p.1 <= selected_row + source_last_row - source_first_row
                    && p.2 >= selected_column
                    && p.2 <= selected_column + source_last_column - source_first_column
                {
                    None
                } else {
                    Some(p)
                }
            });
        }
        // Selection is validated by the prepared footprint; publish history last.
        self.set_selected_range(selected_row, selected_column, max_row, max_column)?;
        self.push_diff_list(diff_list);
        self.evaluate_if_not_paused();
        Ok(())
    }

    /// Paste a csv-string into the model
    pub fn paste_csv_string(&mut self, area: &Area, csv: &str) -> Result<(), String> {
        self.with_user_display_edit(true, |this| {
            this.paste_csv_string_in_display_edit(area, csv)
        })
    }

    fn paste_csv_string_in_display_edit(&mut self, area: &Area, csv: &str) -> Result<(), String> {
        let sheet = area.sheet;

        // First pass: parse all records so we know the full extent before touching any cells.
        let mut records: Vec<Vec<String>> = Vec::new();
        let mut max_width: i32 = 0;
        let csv_reader = Cursor::new(csv);
        let mut reader = ReaderBuilder::new()
            .delimiter(b'\t')
            .has_headers(false)
            .from_reader(csv_reader);
        for r in reader.records().flatten() {
            let row_data: Vec<String> = r.iter().map(|v| v.to_string()).collect();
            max_width = max_width.max(row_data.len() as i32);
            records.push(row_data);
        }
        if records.is_empty() {
            return Ok(());
        }

        // Check whether any static array formula would be partially overwritten.
        let paste_area = Area {
            sheet,
            row: area.row,
            column: area.column,
            width: max_width,
            height: records.len() as i32,
        };

        // Merged cells in the target are replaced by the paste (or reject
        // it); plain text carries no merges, so contained ones just go
        let mut diff_list = Vec::new();
        self.remove_merges_replaced_by_paste(&paste_area, &[], &mut diff_list)?;

        // Capture old values BEFORE clearing so undo can restore them correctly.
        let mut old_values: HashMap<(i32, i32), Option<Cell>> = HashMap::new();
        {
            let ws = self.model.workbook.worksheet(sheet)?;
            for r in area.row..area.row + records.len() as i32 {
                for c in area.column..area.column + max_width {
                    old_values.insert((r, c), ws.cell(r, c).map(Cow::into_owned));
                }
            }
        }

        // Clearing the target area also removes its links: capture them for undo
        diff_list.extend(self.range_link_diffs(&paste_area)?);
        self.model.range_clear_contents(&paste_area)?;

        // Second pass: write values and build diff list.
        let mut row = area.row;
        let mut last_column = area.column;
        for row_data in &records {
            let mut column = area.column;
            for value in row_data {
                let old_value = old_values.remove(&(row, column)).unwrap_or(None);
                diff_list.push(Diff::SetCellValue {
                    sheet,
                    row,
                    column,
                    new_value: value.to_string(),
                    old_value: Box::new(old_value),
                });
                // pasted URLs are auto-linked: capture the link and style diffs too
                self.set_user_input_with_link_diffs(
                    sheet,
                    row,
                    column,
                    value.to_string(),
                    &mut diff_list,
                )?;
                column += 1;
            }
            last_column = last_column.max(column - 1);
            row += 1;
        }
        self.push_diff_list(diff_list);
        // select the pasted area
        self.set_selected_range(area.row, area.column, row - 1, last_column)?;
        self.evaluate_if_not_paused();
        Ok(())
    }
}
