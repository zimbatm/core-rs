use amber_store_core::{
    key::{Key, Type},
    packstore::{Object, Store},
};
use serde_json::json;
use std::{fs, io, path::PathBuf, time::Instant};

fn object(index: u64) -> Object {
    let mut data = vec![0u8; 256];
    data[..8].copy_from_slice(&index.to_le_bytes());
    Object {
        key: Key::new(Type::Blob, data.len() as u64, &data),
        data,
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args().collect();
    if args.len() != 3 {
        return Err("usage: write-throughput NEW_ABSOLUTE_DIRECTORY OBJECT_COUNT".into());
    }
    let directory = PathBuf::from(&args[1]);
    if !directory.is_absolute() {
        return Err("results directory must be absolute".into());
    }
    let count: u64 = args[2].parse()?;
    if count == 0 {
        return Err("object count must be positive".into());
    }
    fs::create_dir(&directory)?;
    for (mode, batch_size) in [("put", 1), ("write_batch", 1024), ("put_unflushed", 0)] {
        let path = directory.join(mode);
        let store = Store::open(&path)?;
        let start = Instant::now();
        if batch_size == 1 {
            for index in 0..count {
                let object = object(index);
                store.put(object.key, &object.data)?;
            }
        } else if batch_size > 0 {
            for first in (0..count).step_by(batch_size) {
                store.write_batch(
                    (first..count.min(first + batch_size as u64))
                        .map(|index| Ok::<_, io::Error>(object(index))),
                )?;
            }
        } else {
            for index in 0..count {
                let object = object(index);
                store.put_unflushed(object.key, &object.data)?;
            }
            store.sync()?;
        }
        let seconds = start.elapsed().as_secs_f64();
        store.close()?;
        let reopened = Store::open(&path)?;
        for index in 0..count {
            let expected = object(index);
            assert_eq!(reopened.get(expected.key)?, expected.data);
        }
        reopened.close()?;
        let result = json!({"mode":mode,"objects":count,"payload_bytes":256,"batch_size":batch_size,
            "seconds":seconds,"objects_per_second":count as f64 / seconds,"reopen":"passed"});
        fs::write(
            directory.join(format!("{mode}.json")),
            serde_json::to_vec_pretty(&result)?,
        )?;
        println!("{result}");
    }
    Ok(())
}
