use calamine::{Data, Reader, open_workbook_auto};
use std::collections::HashMap;
use std::path::Path;
use tracing::{debug, error, info, warn};

pub fn extract_daily_values<P: AsRef<Path>>(
    workbook_path: P,
    sheet_name: &str,
) -> (HashMap<String, f64>, usize) {
    let path = workbook_path.as_ref();
    info!(
        "Extracting daily values from: {:?}, sheet: {}",
        path, sheet_name
    );

    let mut result = HashMap::new();
    let keys = vec!["客户权益", "平仓盈亏", "浮动盈亏", "风险度", "交易手续费"];
    let mut found_keys = 0;

    for key in &keys {
        result.insert(key.to_string(), 0.0);
    }

    let mut workbook = match open_workbook_auto(path) {
        Ok(wb) => {
            debug!("Workbook opened successfully");
            wb
        }
        Err(e) => {
            error!("Failed to open workbook: {}", e);
            return (result, found_keys);
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
            return (result, found_keys);
        }
    };

    for (row_idx, row) in sheet.rows().enumerate() {
        for (col_idx, cell) in row.iter().enumerate() {
            // Stop when we hit the marker
            if let Data::String(s) = cell {
                if s == "期货成交汇总" {
                    debug!("Found end marker '期货成交汇总' at row {}", row_idx);
                    info!(
                        "Extraction completed. Found {} keys: {:?}",
                        found_keys, result
                    );
                    return (result, found_keys);
                }

                if keys.contains(&s.as_str()) {
                    debug!("Found key '{}' at row {}, col {}", s, row_idx, col_idx);

                    // Get value from column +2
                    let value_cell = row.get(col_idx + 2);
                    if let Some(val) = value_cell {
                        let parsed = match val {
                            Data::Float(f) => *f,
                            Data::Int(i) => *i as f64,
                            Data::String(s) => {
                                let clean_str = s.replace(",", "").replace("%", "");
                                match clean_str.parse::<f64>() {
                                    Ok(num) => num,
                                    Err(e) => {
                                        warn!("Failed to parse string '{}' as float: {}", s, e);
                                        // Do not update the result if parse fails
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

    info!(
        "Extraction completed. Found {} keys: {:?}",
        found_keys, result
    );
    (result, found_keys)
}
