use anyhow::{Context, Result};
use bollard::query_parameters::ListContainersOptionsBuilder;
use bollard::Docker;
use tokio::time::{timeout, Duration};

#[tokio::main]
async fn main() -> Result<()> {
    print!(
        "{}",
        "This example lists all running containers.\n\
         Make sure you have a Docker daemon running locally.\n"
    );

    let docker = Docker::connect_with_local_defaults().context("couldn't create Docker client")?;

    print!("{}", "a\n");

    timeout(Duration::from_secs(2), docker.ping())
        .await
        .context("ping timed out")? // Tokio timeout error
        .context("daemon unreachable")?;

    print!("{}", "b\n");

    let mut filters: std::collections::HashMap<String, Vec<String>> =
        std::collections::HashMap::new();
    filters.insert("status".into(), vec!["running".into()]);

    let opts = ListContainersOptionsBuilder::new()
        .all(true) // keep stopped containers too
        .size(false) // don't calculate disk usage
        .filters(&filters) // any &HashMap<String, Vec<String>>
        .build();

    let list = docker.list_containers(Some(opts)).await?;

    for c in list {
        println!("{} → {:?}", c.id.unwrap_or_default(), c.names);
    }

    print!("{}", "c\n");

    Ok(())
}
