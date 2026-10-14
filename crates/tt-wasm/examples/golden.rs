//! Native oracle using the very same public host DTO conversions.
use std::io::{self, Read};
use tt::{Timestamp, host, workspace};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut bytes = Vec::new();
    io::stdin().read_to_end(&mut bytes)?;
    let input: serde_json::Value = serde_json::from_slice(&bytes)?;
    let now: Timestamp = input["now"].as_str().ok_or("now")?.parse()?;
    let mut workspace = workspace::Workspace::memory()?;
    let authority = workspace.session("writer", now)?.stamp.authority;
    let mut outcomes = Vec::new();
    for op in input["operations"].as_array().ok_or("operations")? {
        let operation = host::decode_operation(&serde_json::to_string(op)?)?;
        outcomes.push(serde_json::to_value(
            workspace.apply(&authority, operation, now)?,
        )?);
    }
    let mut projections = Vec::new();
    for query in input["queries"].as_array().ok_or("queries")? {
        projections.push(serde_json::to_value(
            workspace.read(host::decode_query(&serde_json::to_vec(query)?)?, now)?,
        )?);
    }
    println!(
        "{}",
        serde_json::json!({"outcomes": outcomes, "projections": projections})
    );
    Ok(())
}
