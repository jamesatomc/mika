//! Committee group handlers (`committee join|leave`).

use super::CommitteeCmd;
use anyhow::Result;
use mika_core::KanariStorage;

pub async fn run(cmd: CommitteeCmd) -> Result<()> {
    match cmd {
        CommitteeCmd::Join {
            committee_id,
            node_key,
            shard_count,
            password,
        } => {
            let storage = KanariStorage::from_config(&password, "cli-node")?;
            let tx = storage
                .join_committee(&committee_id, &node_key, shard_count)
                .await?;
            eprintln!("Joined committee {} (tx {})", committee_id, tx);
        }
        CommitteeCmd::Leave {
            committee_id,
            node_key,
            password,
        } => {
            let storage = KanariStorage::from_config(&password, "cli-node")?;
            let tx = storage.leave_committee(&committee_id, &node_key).await?;
            eprintln!("Left committee {} (tx {})", committee_id, tx);
        }
    }
    Ok(())
}
