use std::io;
use std::os::unix::io::{AsRawFd, RawFd};
use std::sync::{Arc, Mutex};
use std::vec::Vec;

use crate::phy::{self, sys, Device, DeviceCapabilities, Medium};
use crate::time::Instant;

/// A virtual TUN (IP) or TAP (Ethernet) interface.
#[derive(Debug)]
pub struct TunTapInterface {
    lower: Arc<Mutex<sys::TunTapInterfaceDesc>>,
    mtu: usize,
    medium: Medium,
}

impl AsRawFd for TunTapInterface {
    fn as_raw_fd(&self) -> RawFd {
        self.lower.lock().unwrap().as_raw_fd()
    }
}

unsafe impl Send for sys::TunTapInterfaceDesc {}
unsafe impl Sync for sys::TunTapInterfaceDesc {}

impl TunTapInterface {
    /// Attaches to a TUN/TAP interface called `name`, or creates it if it does not exist.
    ///
    /// If `name` is a persistent interface configured with UID of the current user,
    /// no special privileges are needed. Otherwise, this requires superuser privileges
    /// or a corresponding capability set on the executable.
    pub fn new(name: &str, medium: Medium) -> io::Result<TunTapInterface> {
        let lower = sys::TunTapInterfaceDesc::new(name, medium)?;
        let mtu = lower.interface_mtu()?;
        Ok(TunTapInterface {
            lower: Arc::new(Mutex::new(lower)),
            mtu,
            medium,
        })
    }

    /// Attaches to a TUN/TAP interface specified by file descriptor `fd`.
    ///
    /// On platforms like Android, a file descriptor to a tun interface is exposed.
    /// On these platforms, a TunTapInterface cannot be instantiated with a name.
    pub fn from_fd(fd: RawFd, medium: Medium, mtu: usize) -> io::Result<TunTapInterface> {
        let lower = sys::TunTapInterfaceDesc::from_fd(fd, mtu)?;
        Ok(TunTapInterface {
            lower: Arc::new(Mutex::new(lower)),
            mtu,
            medium,
        })
    }
}

impl Device for TunTapInterface {
    type RxToken<'a> = RxToken;
    type TxToken<'a> = TxToken;

    fn capabilities(&self) -> DeviceCapabilities {
        DeviceCapabilities {
            max_transmission_unit: self.mtu,
            medium: self.medium,
            ..DeviceCapabilities::default()
        }
    }

    fn receive(&mut self, _timestamp: Instant) -> Option<(Self::RxToken<'_>, Self::TxToken<'_>)> {
        let mut lower = self.lower.lock().unwrap();
        let mut buffer = vec![0; self.mtu];
        match lower.recv(&mut buffer[..]) {
            Ok(size) => {
                buffer.resize(size, 0);
                let rx = RxToken { buffer };
                let tx = TxToken {
                    lower: self.lower.clone(),
                };
                Some((rx, tx))
            }
            Err(err) if err.kind() == io::ErrorKind::WouldBlock => None,
            Err(err) => panic!("{}", err),
        }
    }

    fn transmit(&mut self, _timestamp: Instant) -> Option<Self::TxToken<'_>> {
        Some(TxToken {
            lower: self.lower.clone(),
        })
    }
}

#[doc(hidden)]
pub struct RxToken {
    buffer: Vec<u8>,
}

impl phy::RxToken for RxToken {
    fn consume<R, F>(mut self, f: F) -> R
    where
        F: FnOnce(&mut [u8]) -> R,
    {
        println!("\x1b[32mRxToken consume: {:?}\x1b[0m", self.buffer);
        
        f(&mut self.buffer[..])
    }
}

#[doc(hidden)]
pub struct TxToken {
    lower: Arc<Mutex<sys::TunTapInterfaceDesc>>,
}

impl phy::TxToken for TxToken {
    fn consume<R, F>(self, len: usize, f: F) -> R
    where
        F: FnOnce(&mut [u8]) -> R,
    {
        let mut lower = self.lower.lock().unwrap();
        let mut buffer = vec![0; len];
        let result = f(&mut buffer);

        println!("\x1b[32mTxToken consume: {:?}\x1b[0m", buffer);

        match lower.send(&buffer[..]) {
            Ok(_) => {}
            Err(err) if err.kind() == io::ErrorKind::WouldBlock => {
                net_debug!("phy: tx failed due to WouldBlock")
            }
            Err(err) => panic!("{}", err),
        }
        result
    }
}
