use elasticsearch::http::transport::Transport;
use elasticsearch::indices::IndicesCreateParts;
use elasticsearch::{BulkParts, Elasticsearch};
use indicatif::{ProgressBar, ProgressStyle};
use rayon::prelude::*;
use serde::Deserialize;
use std::collections::hash_map::DefaultHasher;
use std::fs;
use std::hash::{Hash, Hasher};
use std::sync::{Arc, Mutex};
use std::thread;

#[derive(Deserialize, Debug)]
struct DriveConfig {
    drives: Drives,
}

#[derive(Deserialize, Debug)]
struct Drives {
    root_paths: Vec<String>,
}

#[derive(Debug)]
struct FileEntry {
    id: u64,
    path: String,
    size: u64,
    parent_id: Option<u64>,
    // ...otros campos si es necesario...
}

#[derive(Debug)]
struct FolderEntry {
    id: u64,
    path: String,
    parent_id: Option<u64>,
    // ...otros campos si es necesario...
}

#[derive(Clone, Debug)]
struct AppState {
    folders: Arc<Mutex<Vec<FolderEntry>>>,
    files: Arc<Mutex<Vec<FileEntry>>>,
}
#[tokio::main]
async fn main() {
    // Read toml config file from DRIVE_CONFIG env variable
    let current_dir = match std::env::current_dir() {
        Ok(dir) => dir,
        Err(_) => {
            panic!("Error: Could not get current directory");
        }
    };

    let config_file_name = match std::env::var("DRIVE_CONFIG") {
        Ok(val) => val,
        Err(_) => {
            panic!("Error: DRIVE_CONFIG env variable not set");
        }
    };

    let config_file_path = current_dir.join(config_file_name);

    let config_file_contents = match std::fs::read_to_string(config_file_path) {
        Ok(file) => file,
        Err(_) => {
            panic!("Error: Could not read config file");
        }
    };

    let config: DriveConfig = match toml::from_str(&config_file_contents) {
        Ok(val) => val,
        Err(_) => {
            panic!("Error: Could not parse config file. Check the format");
        }
    };

    let state: AppState = AppState {
        folders: Arc::new(Mutex::new(Vec::new())),
        files: Arc::new(Mutex::new(Vec::new())),
    };

    let pb = ProgressBar::new_spinner();
    pb.set_style(
        ProgressStyle::default_spinner()
            .template("{spinner:.green} {msg} {pos} items processed")
            .unwrap(),
    );
    pb.set_message("Indexing...");

    let pb = Arc::new(pb);

    // Index files in the root paths
    config.drives.root_paths.par_iter().for_each(|root_path| {
        let state_clone = state.clone();
        let pb_clone = pb.clone();
        let _ = index_path(root_path, state_clone, pb_clone, None);
    });

    pb.finish_with_message("Indexing complete.");

    let elastic_pb = ProgressBar::new_spinner();

    // Now connect to the elasticsearch server and upload the indexed files.
    let elastic_url = match std::env::var("ELASTICSEARCH_URL") {
        Ok(val) => val,
        Err(_) => {
            panic!("Error: ELASTIC_URL env variable not set");
        }
    };
    pb.set_message("Connecting to elasticsearch...");
    let transport = Transport::single_node(&elastic_url).expect("Failed to create transport");
    let client = Elasticsearch::new(transport);
    pb.set_message("Creating index...");
    let index_response = client
        .indices()
        .create(IndicesCreateParts::Index("files_index"))
        .send()
        .await
        .expect("Failed to create elasticsearch index");
    pb.set_message("Uploading files...");
}

fn index_path(
    path: &str,
    app_state: AppState,
    pb: Arc<ProgressBar>,
    parent_id: Option<u64>,
) -> Result<(), ()> {
    let metadata = match fs::metadata(path) {
        Ok(meta) => meta,
        Err(_) => {
            return Err(());
        }
    };
    let mut hasher = DefaultHasher::new();
    path.hash(&mut hasher);
    let id = hasher.finish();

    if metadata.is_dir() {
        let folder = FolderEntry {
            id,
            path: path.to_string(),
            parent_id,
        };
        {
            app_state
                .folders
                .lock()
                .expect("Failed to add folder to mutex array.")
                .push(folder);
        }
        pb.inc(1);
        let entries = fs::read_dir(path).unwrap();
        let entries_handlers = entries.map(|entry| {
            let entry = entry.unwrap();
            let path = entry.path();
            let path_str = path.to_str().unwrap().to_owned();
            let app_state = app_state.clone();
            let pb = pb.clone();
            let parent_id = Some(id);
            thread::spawn(move || {
                let _ = index_path(&path_str, app_state, pb, parent_id);
            })
        });
        entries_handlers.for_each(|h| {
            match h.join() {
                Ok(_) => {}
                Err(_) => {}
            };
        });
    } else if metadata.is_file() {
        let file = FileEntry {
            id,
            path: path.to_string(),
            size: metadata.len(),
            parent_id,
        };
        {
            app_state
                .files
                .lock()
                .expect("Failed to add file to mutex array.")
                .push(file);
        }
        pb.inc(1);
    }
    Ok(())
}
