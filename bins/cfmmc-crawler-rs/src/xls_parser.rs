use calamine::{Data, Reader, open_workbook_auto};
use std::collections::HashMap;
use std::path::Path;
use tracing::{debug, error, info, warn};

const SETTLEMENT_KEYS: [&str; 5] = ["客户权益", "平仓盈亏", "浮动盈亏", "风险度", "交易手续费"];

fn log_extraction_summary(found_keys: usize, result: &HashMap<String, f64>) {
    // Emit only key counts at info!; financial amounts must stay at debug! level.
    info!("Extraction completed. Found {} keys", found_keys);
    debug!("Extracted daily values: {:?}", result);
}

/// Extract settlement values from the workbook.
///
/// Returns `None` when the workbook or sheet cannot be read at all, so
/// callers can distinguish "extraction failed" from "values are zero".
pub fn extract_daily_values<P: AsRef<Path>>(
    workbook_path: P,
    sheet_name: &str,
) -> Option<(HashMap<String, f64>, usize)> {
    let path = workbook_path.as_ref();
    info!(
        "Extracting daily values from: {:?}, sheet: {}",
        path, sheet_name
    );

    let mut result = HashMap::new();
    let mut found_keys = 0;

    for key in SETTLEMENT_KEYS {
        result.insert(key.to_string(), 0.0);
    }

    let mut workbook = match open_workbook_auto(path) {
        Ok(wb) => {
            debug!("Workbook opened successfully");
            wb
        }
        Err(e) => {
            error!("Failed to open workbook: {}", e);
            return None;
        }
    };

    let sheet = match workbook.worksheet_range(sheet_name) {
        Ok(range) => {
            debug!(
                "Sheet '{}' found with dimensions: {:?}",
                sheet_name,
                range.get_size()
            );
            range
        }
        Err(e) => {
            error!("Failed to access sheet '{}': {}", sheet_name, e);
            return None;
        }
    };

    for (row_idx, row) in sheet.rows().enumerate() {
        for (col_idx, cell) in row.iter().enumerate() {
            // Stop when we hit the marker
            if let Data::String(s) = cell {
                if s == "期货成交汇总" {
                    debug!("Found end marker '期货成交汇总' at row {}", row_idx);
                    log_extraction_summary(found_keys, &result);
                    return Some((result, found_keys));
                }

                if SETTLEMENT_KEYS.contains(&s.as_str()) {
                    debug!("Found key '{}' at row {}, col {}", s, row_idx, col_idx);

                    // Get value from column +2
                    let value_cell = row.get(col_idx + 2);
                    if let Some(val) = value_cell {
                        let parsed = match val {
                            Data::Float(f) => *f,
                            Data::Int(i) => *i as f64,
                            Data::String(raw) => {
                                let clean_str = raw.replace([',', '%'], "");
                                match clean_str.trim().parse::<f64>() {
                                    Ok(num) => num,
                                    Err(e) => {
                                        warn!("Failed to parse string '{}' as float: {}", raw, e);
                                        continue;
                                    }
                                }
                            }
                            _ => {
                                warn!("Unsupported cell type for key '{}': {:?}", s, val);
                                continue;
                            }
                        };

                        debug!("Extracted value for '{}': {}", s, parsed);
                        result.insert(s.clone(), parsed);
                        found_keys += 1;
                    } else {
                        warn!("No value found for key '{}' at expected column", s);
                    }
                }
            }
        }
    }

    log_extraction_summary(found_keys, &result);
    Some((result, found_keys))
}

#[cfg(test)]
pub(crate) mod test_helpers {
    use std::io::Write;

    /// Build a minimal uncompressed `.xlsx` (OOXML ZIP with STORE compression)
    /// containing a single sheet named `sheet_name` and rows of `(col_letter, value)` cells.
    pub fn build_minimal_xlsx(sheet_name: &str, rows: &[Vec<(&str, &str)>]) -> Vec<u8> {
        let mut sheet_xml = String::from(
            r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><sheetData>"#,
        );
        for (idx, row) in rows.iter().enumerate() {
            let row_num = idx + 1;
            sheet_xml.push_str(&format!(r#"<row r="{row_num}">"#));
            for (col, val) in row {
                let escaped = val
                    .replace('&', "&amp;")
                    .replace('<', "&lt;")
                    .replace('>', "&gt;");
                sheet_xml.push_str(&format!(
                    r#"<c r="{col}{row_num}" t="inlineStr"><is><t>{escaped}</t></is></c>"#
                ));
            }
            sheet_xml.push_str("</row>");
        }
        sheet_xml.push_str("</sheetData></worksheet>");

        let content_types = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/><Override PartName="/xl/worksheets/sheet1.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/></Types>"#;
        let root_rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="xl/workbook.xml"/></Relationships>"#;
        let wb_rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet1.xml"/></Relationships>"#;
        let workbook_xml = format!(
            r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><sheets><sheet name="{sheet_name}" sheetId="1" r:id="rId1"/></sheets></workbook>"#
        );

        let entries: [(&str, &[u8]); 5] = [
            ("[Content_Types].xml", content_types.as_bytes()),
            ("_rels/.rels", root_rels.as_bytes()),
            ("xl/_rels/workbook.xml.rels", wb_rels.as_bytes()),
            ("xl/workbook.xml", workbook_xml.as_bytes()),
            ("xl/worksheets/sheet1.xml", sheet_xml.as_bytes()),
        ];

        build_store_zip(&entries)
    }

    fn crc32(data: &[u8]) -> u32 {
        let mut crc: u32 = 0xFFFF_FFFF;
        for &byte in data {
            crc ^= u32::from(byte);
            for _ in 0..8 {
                let mask = (crc & 1).wrapping_neg();
                crc = (crc >> 1) ^ (0xEDB8_8320 & mask);
            }
        }
        !crc
    }

    fn build_store_zip(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut out = Vec::new();
        let mut central = Vec::new();
        for &(name, data) in entries {
            let offset = out.len() as u32;
            let name_bytes = name.as_bytes();
            let crc = crc32(data);
            let size = data.len() as u32;

            // Local file header
            out.write_all(&0x0403_4B50u32.to_le_bytes()).unwrap();
            out.write_all(&20u16.to_le_bytes()).unwrap();
            out.write_all(&0u16.to_le_bytes()).unwrap();
            out.write_all(&0u16.to_le_bytes()).unwrap(); // STORE
            out.write_all(&0u16.to_le_bytes()).unwrap();
            out.write_all(&0u16.to_le_bytes()).unwrap();
            out.write_all(&crc.to_le_bytes()).unwrap();
            out.write_all(&size.to_le_bytes()).unwrap();
            out.write_all(&size.to_le_bytes()).unwrap();
            out.write_all(&(name_bytes.len() as u16).to_le_bytes())
                .unwrap();
            out.write_all(&0u16.to_le_bytes()).unwrap();
            out.write_all(name_bytes).unwrap();
            out.write_all(data).unwrap();

            // Central directory header
            central.write_all(&0x0201_4B50u32.to_le_bytes()).unwrap();
            central.write_all(&20u16.to_le_bytes()).unwrap();
            central.write_all(&20u16.to_le_bytes()).unwrap();
            central.write_all(&0u16.to_le_bytes()).unwrap();
            central.write_all(&0u16.to_le_bytes()).unwrap();
            central.write_all(&0u16.to_le_bytes()).unwrap();
            central.write_all(&0u16.to_le_bytes()).unwrap();
            central.write_all(&crc.to_le_bytes()).unwrap();
            central.write_all(&size.to_le_bytes()).unwrap();
            central.write_all(&size.to_le_bytes()).unwrap();
            central
                .write_all(&(name_bytes.len() as u16).to_le_bytes())
                .unwrap();
            central.write_all(&0u16.to_le_bytes()).unwrap();
            central.write_all(&0u16.to_le_bytes()).unwrap();
            central.write_all(&0u16.to_le_bytes()).unwrap();
            central.write_all(&0u16.to_le_bytes()).unwrap();
            central.write_all(&0u32.to_le_bytes()).unwrap();
            central.write_all(&offset.to_le_bytes()).unwrap();
            central.write_all(name_bytes).unwrap();
        }

        let cd_offset = out.len() as u32;
        let cd_size = central.len() as u32;
        let count = entries.len() as u16;
        out.extend_from_slice(&central);

        // End of central directory
        out.write_all(&0x0605_4B50u32.to_le_bytes()).unwrap();
        out.write_all(&0u16.to_le_bytes()).unwrap();
        out.write_all(&0u16.to_le_bytes()).unwrap();
        out.write_all(&count.to_le_bytes()).unwrap();
        out.write_all(&count.to_le_bytes()).unwrap();
        out.write_all(&cd_size.to_le_bytes()).unwrap();
        out.write_all(&cd_offset.to_le_bytes()).unwrap();
        out.write_all(&0u16.to_le_bytes()).unwrap();
        out
    }
}

#[cfg(test)]
mod tests {
    use super::extract_daily_values;
    use super::test_helpers::build_minimal_xlsx;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn unique_temp_path(name: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("cfmmc_xls_test_{name}_{nanos}.xlsx"))
    }

    #[test]
    fn extract_daily_values_returns_none_for_corrupt_workbook() {
        let path = unique_temp_path("corrupt");
        std::fs::write(&path, b"<html><body>Session expired</body></html>").unwrap();
        let res = extract_daily_values(&path, "客户交易结算日报");
        let _ = std::fs::remove_file(&path);
        assert!(res.is_none());
    }

    #[test]
    fn extract_daily_values_parses_settlement_sheet_and_stops_at_marker() {
        let path = unique_temp_path("valid");
        let xlsx = build_minimal_xlsx(
            "客户交易结算日报",
            &[
                vec![("A", "客户权益"), ("C", "123,456.78")],
                vec![("A", "平仓盈亏"), ("C", "-500.25")],
                vec![("A", "风险度"), ("C", "12.50%")],
                vec![("A", "期货成交汇总"), ("C", "ignored")],
                vec![("A", "交易手续费"), ("C", "999.00")],
            ],
        );
        std::fs::write(&path, xlsx).unwrap();

        let (values, count) = extract_daily_values(&path, "客户交易结算日报").unwrap();
        let _ = std::fs::remove_file(&path);

        assert_eq!(count, 3);
        assert!((values["客户权益"] - 123_456.78).abs() < 1e-6);
        assert!((values["平仓盈亏"] - (-500.25)).abs() < 1e-6);
        assert!((values["风险度"] - 12.50).abs() < 1e-6);
        // Should stop before 期货成交汇总, so 交易手续费 remains 0.0
        assert!((values["交易手续费"] - 0.0).abs() < 1e-6);
    }
}
