//! Exclusive OS-backed profile lock, automatically released on process death.
//! Never unlink the lock file: another process may have an open handle to it.
use std::{fs::{File,OpenOptions},path::Path};
pub struct ProfileLock { _file:File }
impl ProfileLock {
    pub fn acquire(root:&Path)->anyhow::Result<Self>{
        std::fs::create_dir_all(root)?;
        let path=root.join("wetube.profile.lock");
        if let Ok(meta)=std::fs::symlink_metadata(&path) {
            anyhow::ensure!(meta.is_file()&&!meta.file_type().is_symlink(),"Unsafe profile lock");
        }
        let file=OpenOptions::new().read(true).write(true).create(true).truncate(false).open(path)?;
        file.try_lock().map_err(|e|anyhow::anyhow!("Profile lock unavailable: {e:?}"))?;
        Ok(Self{_file:file})
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn profile_lock_excludes_second_owner(){
        let root=std::env::temp_dir().join(format!("wetube-lock-{:032x}",rand::random::<u128>()));
        let first=ProfileLock::acquire(&root).unwrap();
        assert!(ProfileLock::acquire(&root).is_err());
        drop(first);
        let second=ProfileLock::acquire(&root).unwrap();drop(second);
        std::fs::remove_dir_all(root).unwrap();
    }
}
