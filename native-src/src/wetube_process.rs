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
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            // CREATE_SUSPENDED | CREATE_NO_WINDOW: no application code before assignment.
            cmd.creation_flags(0x00000004 | 0x08000000);
        }
        let mut child=cmd.spawn()?;
        #[cfg(windows)]
        if let Err(e)=job.assign(&child).and_then(|_| win::resume_primary(child.id())) {
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
        fn CreateToolhelp32Snapshot(flags:u32,pid:u32)->Handle;
        fn Thread32First(snapshot:Handle,entry:*mut ThreadEntry)->i32;
        fn Thread32Next(snapshot:Handle,entry:*mut ThreadEntry)->i32;
        fn OpenThread(access:u32,inherit:i32,id:u32)->Handle;
        fn ResumeThread(thread:Handle)->u32;
        fn GetProcessIdOfThread(thread:Handle)->u32;
        fn QueryInformationJobObject(job:Handle,class:i32,info:*mut c_void,len:u32,ret:*mut u32)->i32;
    }
    #[repr(C)]
    struct ThreadEntry { size:u32, usage:u32, thread:u32, owner:u32, base:i32, delta:i32, flags:u32 }
    struct OwnedHandle(Handle);
    impl Drop for OwnedHandle {fn drop(&mut self){unsafe{CloseHandle(self.0);}}}
    /// Rust Child retains the process handle but not the primary thread handle.
    /// Enumerate the suspended process's single thread; fail closed if ambiguous.
    pub fn resume_primary(pid:u32)->io::Result<()> {
        let raw=unsafe{CreateToolhelp32Snapshot(0x4,0)};
        if raw as isize == -1 {return Err(io::Error::last_os_error());}
        let snapshot=OwnedHandle(raw);
        let mut entry:ThreadEntry=unsafe{std::mem::zeroed()};
        entry.size=std::mem::size_of::<ThreadEntry>() as u32;
        let mut found=None;
        let mut ok=unsafe{Thread32First(snapshot.0,&mut entry)};
        if ok==0 {return Err(io::Error::last_os_error());}
        while ok!=0 {
            if entry.owner==pid {
                if found.replace(entry.thread).is_some(){return Err(io::Error::other("Suspended child has multiple threads"));}
            }
            entry.size=std::mem::size_of::<ThreadEntry>() as u32;
            ok=unsafe{Thread32Next(snapshot.0,&mut entry)};
        }
        let last=io::Error::last_os_error();
        if last.raw_os_error()!=Some(18){return Err(last);}
        let id=found.ok_or_else(||io::Error::other("Primary thread not found"))?;
        // THREAD_SUSPEND_RESUME | THREAD_QUERY_LIMITED_INFORMATION
        let raw=unsafe{OpenThread(0x0002|0x0800,0,id)};
        if raw.is_null(){return Err(io::Error::last_os_error());}
        let thread=OwnedHandle(raw);
        if unsafe{GetProcessIdOfThread(thread.0)}!=pid {return Err(io::Error::other("Primary thread owner changed"));}
        let previous=unsafe{ResumeThread(thread.0)};
        if previous!=1 {return Err(io::Error::other("Unexpected primary thread suspend count"));}
        Ok(())
    }
    #[repr(C)]
    struct Accounting {
        total_user:i64,total_kernel:i64,period_user:i64,period_kernel:i64,
        faults:u32,total_processes:u32,active_processes:u32,terminated_processes:u32,
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
            // The primary thread is still suspended here.
            if unsafe{AssignProcessToJobObject(self.0,child.as_raw_handle().cast())}==0 {Err(io::Error::last_os_error())}else{Ok(())}
        }
        pub fn terminate(&self){unsafe{TerminateJobObject(self.0,1);}}
        pub fn counts(&self)->io::Result<(u32,u32)>{
            let mut info:Accounting=unsafe{std::mem::zeroed()};
            if unsafe{QueryInformationJobObject(self.0,1,(&mut info as *mut Accounting).cast(),std::mem::size_of::<Accounting>() as u32,std::ptr::null_mut())}==0{return Err(io::Error::last_os_error());}
            Ok((info.total_processes,info.active_processes))
        }
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
    #[test]
    fn terminates_descendant_tree() {
        let mut cmd=Command::new("cmd.exe");
        // cmd runs a child ping, which remains alive until the Job is terminated.
        cmd.args(["/d","/c","ping -n 60 127.0.0.1 >nul"]);
        let mut child=ScopedChild::spawn(&mut cmd).unwrap();
        let deadline=std::time::Instant::now()+std::time::Duration::from_secs(8);
        loop {
            let (_,active)=child.job.counts().unwrap();
            if active>=2{break;}
            assert!(std::time::Instant::now()<deadline,"No descendant observed");
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        child.terminate();
        let deadline=std::time::Instant::now()+std::time::Duration::from_secs(8);
        loop {
            let (total,active)=child.job.counts().unwrap();
            assert!(total>=2);
            if active==0{break;}
            assert!(std::time::Instant::now()<deadline,"Descendants survived cancellation");
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
    }
    #[test]
    fn assigned_before_child_runs() {
        let mut cmd=Command::new("cmd.exe");
        cmd.args(["/d","/c","echo wetube-job-ready"]).stdout(std::process::Stdio::piped());
        let mut child=ScopedChild::spawn(&mut cmd).unwrap();
        let mut out=String::new();
        use std::io::Read;
        child.stdout.take().unwrap().read_to_string(&mut out).unwrap();
        assert!(child.wait().unwrap().success());
        assert!(out.contains("wetube-job-ready"));
    }
}
