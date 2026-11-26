//! Owned media subprocess; Windows Job Object limits descendant lifetime.
use std::{ops::{Deref,DerefMut},process::{Child,Command}};
pub struct ScopedChild {
    child: Child,
    #[cfg(windows)]
    job: win::Job,
}
impl ScopedChild {
    pub fn spawn(cmd:&mut Command)->std::io::Result<Self> {
        #[cfg(windows)]
        let job=win::Job::new()?;
        let mut child=cmd.spawn()?;
        #[cfg(windows)]
        if let Err(e)=job.assign(&child) {
            let _=child.kill(); let _=child.wait(); return Err(e);
        }
        Ok(Self{child,#[cfg(windows)] job})
    }
    pub fn terminate(&mut self) {
        #[cfg(windows)]
        self.job.terminate();
        let _=self.child.kill(); let _=self.child.wait();
    }
}
impl Deref for ScopedChild { type Target=Child; fn deref(&self)->&Child { &self.child } }
impl DerefMut for ScopedChild { fn deref_mut(&mut self)->&mut Child { &mut self.child } }
impl Drop for ScopedChild { fn drop(&mut self) { self.terminate(); } }
#[cfg(windows)]
mod win {
    use std::{ffi::c_void,io,os::windows::io::AsRawHandle};
    type Handle=*mut c_void;
    #[repr(C)]
    struct Basic {
        process_time:i64, job_time:i64, flags:u32, min_ws:usize,max_ws:usize,
        active_limit:u32, affinity:usize, priority:u32, scheduling:u32,
    }
    #[repr(C)]
    struct Counters { read_ops:u64,write_ops:u64,other_ops:u64,read_bytes:u64,write_bytes:u64,other_bytes:u64 }
    #[repr(C)]
    struct Extended { basic:Basic,io:Counters,process_memory:usize,job_memory:usize,peak_process:usize,peak_job:usize }
    #[link(name="kernel32")]
    unsafe extern "system" {
        fn CreateJobObjectW(attributes:*const c_void,name:*const u16)->Handle;
        fn SetInformationJobObject(job:Handle,class:i32,info:*const c_void,len:u32)->i32;
        fn AssignProcessToJobObject(job:Handle,process:Handle)->i32;
        fn TerminateJobObject(job:Handle,code:u32)->i32;
        fn CloseHandle(handle:Handle)->i32;
    }
    pub struct Job(Handle);
    impl Job {
        pub fn new()->io::Result<Self> {
            let h=unsafe{CreateJobObjectW(std::ptr::null(),std::ptr::null())};
            if h.is_null(){return Err(io::Error::last_os_error());}
            let job=Self(h);
            let mut info:Extended=unsafe{std::mem::zeroed()};
            info.basic.flags=0x2000; // JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE
            if unsafe{SetInformationJobObject(h,9,(&info as *const Extended).cast(),std::mem::size_of::<Extended>() as u32)}==0 {
                return Err(io::Error::last_os_error());
            }
            Ok(job)
        }
        pub fn assign(&self,child:&std::process::Child)->io::Result<()> {
            // Immediate assignment after spawn; a descendant created before assignment is not guaranteed.
            if unsafe{AssignProcessToJobObject(self.0,child.as_raw_handle().cast())}==0 {Err(io::Error::last_os_error())}else{Ok(())}
        }
        pub fn terminate(&self){unsafe{TerminateJobObject(self.0,1);}}
    }
    impl Drop for Job {fn drop(&mut self){unsafe{CloseHandle(self.0);}}}
}
#[cfg(all(test,windows))]
mod tests {
    use super::*;
    #[test] fn cancels_owned_process() {
        let mut cmd=Command::new("cmd.exe");
        cmd.args(["/d","/c","ping -n 30 127.0.0.1 >nul"]);
        crate::api::streams::hide_window(&mut cmd);
        let mut child=ScopedChild::spawn(&mut cmd).unwrap();
        child.terminate();
        assert!(child.try_wait().unwrap().is_some());
    }
}
