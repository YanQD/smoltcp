extern crate alloc;

use core::marker::PhantomData;
use alloc::boxed::Box;
use smoltcp::{phy::{Device, DeviceCapabilities, Medium, RxToken, TunTapInterface, TxToken}, time::Instant};

pub trait ObjectSafeDeviceOps {
    fn capabilities(&self) -> DeviceCapabilities;
    fn receive<'a>(&'a mut self, timestamp: Instant) -> Option<(Box<dyn ObjectSafeRxTokenOps<'a> + 'a>, Box<dyn ObjectSafeTxTokenOps<'a> + 'a>)>;
    fn transmit<'a>(&'a mut self, timestamp: Instant) -> Option<Box<dyn ObjectSafeTxTokenOps<'a> + 'a>>;
}

pub struct DeviceWrapper {
    pub inner: Box<dyn ObjectSafeDeviceOps>,
}

impl DeviceWrapper {
    pub fn new(inner: Box<dyn ObjectSafeDeviceOps>) -> Self {
        DeviceWrapper { 
            inner
        }
    }

    pub fn into_inner(self) -> Box<dyn ObjectSafeDeviceOps> {
        self.inner
    }
}

impl Device for DeviceWrapper {
    type RxToken<'a> = RxTokenWrapper<'a> where Self: 'a;
    type TxToken<'a> = TxTokenWrapper<'a> where Self: 'a;

    fn capabilities(&self) -> DeviceCapabilities {
        self.inner.capabilities()
    }

    fn receive(&mut self, timestamp: Instant) -> Option<(Self::RxToken<'_>, Self::TxToken<'_>)> {
        self.inner.receive(timestamp).map(|(rx, tx)| {(
            RxTokenWrapper { rx },
            TxTokenWrapper { tx }
        )})
    }

    fn transmit(&mut self, timestamp: Instant) -> Option<Self::TxToken<'_>> {
        self.inner.transmit(timestamp).map(|tx| 
            TxTokenWrapper { tx }
        )
    }
}

pub trait ObjectSafeRxTokenOps<'a> {
    fn consume_with(&mut self, f: &mut dyn FnMut(&mut [u8]));
}

pub trait ObjectSafeTxTokenOps<'a> {
    fn consume_with(&mut self, len: usize, f: &mut dyn FnMut(&mut [u8]));
}

pub struct ObjectSafeDevice<D: Device> {
    inner: D,
}

impl<D: Device> ObjectSafeDevice<D> {
    pub fn new(device: D) -> Self {
        ObjectSafeDevice { 
            inner: device, 
        }
    }
}

impl<D: Device> Device for ObjectSafeDevice<D> {
    type RxToken<'a> = ObjectSafeRxToken<'a, D::RxToken<'a>> where Self: 'a;
    type TxToken<'a> = ObjectSafeTxToken<'a, D::TxToken<'a>> where Self: 'a;

    fn capabilities(&self) -> DeviceCapabilities {
        self.inner.capabilities() 
    }

    fn receive(&mut self, timestamp: Instant) -> Option<(Self::RxToken<'_>, Self::TxToken<'_>)> {
        self.inner.receive(timestamp).map(|(rx, tx)| {
            (ObjectSafeRxToken::new(rx), ObjectSafeTxToken::new(tx))
        })
    }

    fn transmit(&mut self, timestamp: Instant) -> Option<Self::TxToken<'_>> {
        self.inner.transmit(timestamp).map(ObjectSafeTxToken::new)
    }
}

impl<D: Device> ObjectSafeDeviceOps for ObjectSafeDevice<D> {
    fn capabilities(&self) -> DeviceCapabilities {
        Device::capabilities(self)
    }

    fn receive<'a>(&'a mut self, timestamp: Instant) -> Option<(Box<dyn ObjectSafeRxTokenOps<'a> + 'a>, Box<dyn ObjectSafeTxTokenOps<'a> + 'a>)> {
        // println!("ObjectSafeDeviceOps receive called");
        Device::receive(self, timestamp).map(|(rx, tx)| {(
            Box::new(rx) as Box<dyn ObjectSafeRxTokenOps + 'a>,
            Box::new(tx) as Box<dyn ObjectSafeTxTokenOps + 'a>
        )})
    }

    fn transmit<'a>(&'a mut self, timestamp: Instant) -> Option<Box<dyn ObjectSafeTxTokenOps<'a> + 'a>> {
        // println!("ObjectSafeDeviceOps transmit called");
        Device::transmit(self, timestamp).map(|tx| Box::new(tx) as Box<dyn ObjectSafeTxTokenOps + 'a>)
    }
}

pub struct ObjectSafeRxToken<'a, R: RxToken + 'a> {
    inner: Option<R>,
    _phantom: PhantomData<&'a ()>,
}

impl<'a, R: RxToken + 'a> ObjectSafeRxToken<'a, R> {
    fn new(token: R) -> Self {
        ObjectSafeRxToken {
            inner: Some(token),
            _phantom: PhantomData,
        }
    }
}

impl<'a, R: RxToken + 'a> RxToken for ObjectSafeRxToken<'a, R> {
    fn consume<T, F>(mut self, f: F) -> T
    where
        F: FnOnce(&mut [u8]) -> T,
    {
        self.inner.take().unwrap().consume(f)
    }
}

impl<'a, R: RxToken + 'a> ObjectSafeRxTokenOps<'a> for ObjectSafeRxToken<'a, R> {
    fn consume_with(&mut self, f: &mut dyn FnMut(&mut [u8])) {
        if let Some(token) = self.inner.take() {
            token.consume(|buffer| f(buffer));
        }
    }
}

pub struct RxTokenWrapper<'a> {
    pub rx: Box<dyn ObjectSafeRxTokenOps<'a> + 'a>,
}

impl<'a> RxToken for RxTokenWrapper<'a> {
    fn consume<R, F>(mut self, f: F) -> R
    where
        F: FnOnce(&mut [u8]) -> R,
    {
        let mut result = None;
        let mut f = Some(f);
        self.rx.consume_with(&mut |buffer| {
            if let Some(f) = f.take() {
                result = Some(f(buffer));
            }
        });
        result.unwrap()
    }
}

pub struct ObjectSafeTxToken<'a, T: TxToken + 'a> {
    inner: Option<T>,
    _phantom: PhantomData<&'a ()>,
}

impl<'a, T: TxToken + 'a> ObjectSafeTxToken<'a, T> {
    fn new(token: T) -> Self {
        ObjectSafeTxToken {
            inner: Some(token),
            _phantom: PhantomData,
        }
    }
}

impl<'a, T: TxToken + 'a> TxToken for ObjectSafeTxToken<'a, T> {
    fn consume<R, F>(mut self, len: usize, f: F) -> R
    where
        F: FnOnce(&mut [u8]) -> R,
    {
        self.inner.take().unwrap().consume(len, f)
    }
}

impl<'a, T: TxToken + 'a> ObjectSafeTxTokenOps<'a> for ObjectSafeTxToken<'a, T> {
    fn consume_with(&mut self, len: usize, f: &mut dyn FnMut(&mut [u8])) {
        if let Some(token) = self.inner.take() {
            token.consume(len, |buffer| f(buffer));
        }
    }
}

pub struct TxTokenWrapper<'a> {
    pub tx: Box<dyn ObjectSafeTxTokenOps<'a> + 'a>,
}

impl<'a> TxTokenWrapper<'a> {
    pub fn new(token: Box<dyn ObjectSafeTxTokenOps<'a> + 'a>) -> Self {
        TxTokenWrapper { 
            tx: token
        }
    }
}

impl<'a> TxToken for TxTokenWrapper<'a> {
    fn consume<R, F>(mut self, len: usize, f: F) -> R
    where
        F: FnOnce(&mut [u8]) -> R,
    {
        // println!("TxTokenWrapper consume called");
        let mut result = None;
        let mut f = Some(f);
        self.tx.consume_with(len, &mut |buffer| {
            if let Some(f) = f.take() {
                result = Some(f(buffer));
            }
        });
        result.expect("consume_with should have called the closure")
    }
}

pub struct BridgeDevice {
    pub inner: Box<dyn ObjectSafeDeviceOps>,
}

impl BridgeDevice {
    pub fn new<D: Device + 'static>(device: D) -> Self {
        BridgeDevice {
            inner: Box::new(ObjectSafeDevice::new(device)),
        }
    }

    pub fn transmit_with(&mut self, timestamp: Instant) -> Option<Box<dyn ObjectSafeTxTokenOps + '_>> {
        self.inner.transmit(timestamp)
    }

    pub fn receive_with(&mut self, timestamp: Instant) -> Option<(Box<dyn ObjectSafeRxTokenOps + '_>, Box<dyn ObjectSafeTxTokenOps + '_>)> {
        self.inner.receive(timestamp)
    }

    pub fn capabilities_with(&self) -> DeviceCapabilities {
        self.inner.capabilities()
    }

    pub fn get_inner_mut(&mut self) -> &mut Box<dyn ObjectSafeDeviceOps> {
        &mut self.inner
    }

    pub fn get_inner(&self) -> &Box<dyn ObjectSafeDeviceOps> {
        &self.inner
    }

    pub fn into_inner(self) -> Box<dyn ObjectSafeDeviceOps> {
        self.inner
    }

    pub fn from_box(device: Box<dyn ObjectSafeDeviceOps>) -> Self {
        Self { 
            inner: device 
        }
    }

    pub fn from_object_safe(device: Box<dyn ObjectSafeDeviceOps>) -> Self {
        BridgeDevice { inner: device }
    }

    pub fn as_mut_bridge_device(device: &mut Box<dyn ObjectSafeDeviceOps>) -> &mut BridgeDevice {
        // 这是不安全的，因为我们在进行类型转换
        // 只有当我们确定 Box<dyn ObjectSafeDeviceOps> 确实是 BridgeDevice 时才能这样做
        unsafe { &mut *(device as *mut Box<dyn ObjectSafeDeviceOps> as *mut BridgeDevice) }
    }
}

impl Device for BridgeDevice {
    type RxToken<'a> = RxTokenWrapper<'a> where Self: 'a;
    type TxToken<'a> = TxTokenWrapper<'a> where Self: 'a;

    fn capabilities(&self) -> DeviceCapabilities {
        self.inner.capabilities()
    }

    fn receive(&mut self, timestamp: Instant) -> Option<(Self::RxToken<'_>, Self::TxToken<'_>)> {
        self.inner.receive(timestamp).map(|(rx, tx)| {(
            RxTokenWrapper { rx },
            TxTokenWrapper { tx }
        )})
    }

    fn transmit(&mut self, timestamp: Instant) -> Option<Self::TxToken<'_>> {
        self.inner.transmit(timestamp).map(|tx| 
            TxTokenWrapper { tx }
        )
    }
}

unsafe impl Send for BridgeDevice {}
unsafe impl Sync for BridgeDevice {}

// Helper function to create a boxed ObjectSafeDeviceOps
pub fn boxed_object_safe_device<D: Device + 'static>(device: D) -> Box<dyn ObjectSafeDeviceOps> {
    Box::new(ObjectSafeDevice::new(device))
}

#[cfg(test)]
mod tests {
    use super::*;
    use log::debug;
    use crate::phy::{Loopback, TunTapInterface, Medium};

    #[test]
    fn test_compatible_object_safe_devices() {
        debug!("Testing compatible object-safe devices");

        // Create a Loopback device
        let loopback = Loopback::new(Medium::Ethernet);
        let mut object_safe_loopback = ObjectSafeDevice::new(loopback);

        // Test static dispatch (Device trait)
        let loopback_capabilities = Device::capabilities(&object_safe_loopback);
        assert_eq!(loopback_capabilities.medium, Medium::Ethernet);

        if let Some((rx, tx)) = Device::receive(&mut object_safe_loopback, Instant::now()) {
            rx.consume(|buffer| {
                debug!("Received {} bytes (static dispatch)", buffer.len());
            });
            tx.consume(64, |buffer| {
                buffer.fill(0);
                debug!("Transmitted {} bytes (static dispatch)", buffer.len());
            });
        }

        // Test dynamic dispatch (ObjectSafeDeviceOps trait)
        let mut boxed_loopback: Box<dyn ObjectSafeDeviceOps> = boxed_object_safe_device(Loopback::new(Medium::Ethernet));
        let boxed_loopback_capabilities = boxed_loopback.capabilities();
        assert_eq!(boxed_loopback_capabilities.medium, Medium::Ethernet);

        if let Some((mut rx, mut tx)) = boxed_loopback.receive(Instant::now()) {
            rx.consume_with(&mut |buffer| {
                println!("Received {} bytes (dynamic dispatch)", buffer.len());
            });
            tx.consume_with(64, &mut |buffer| {
                buffer.fill(0);
                debug!("Transmitted {} bytes (dynamic dispatch)", buffer.len());
            });
        }

        // Create a TunTapInterface device (assuming it exists and can be created this way)
        // Note: This is a placeholder and may need to be adjusted based on your actual TunTapInterface implementation
        let tun = TunTapInterface::new("tun0", Medium::Ethernet).expect("Failed to create TUN device");
        let mut boxed_tun: &mut Box<dyn ObjectSafeDeviceOps> = &mut boxed_object_safe_device(tun);

        // Demonstrate usage with multiple device types
        let devices: Vec<&mut Box<dyn ObjectSafeDeviceOps>> = vec![&mut boxed_loopback, &mut boxed_tun];

        for device in devices {
            println!("Device capabilities: {:?}", device.capabilities());
            
            if let Some((mut rx, mut tx)) = device.receive(Instant::now()) {
                rx.consume_with(&mut |buffer| {
                    println!("Received {} bytes", buffer.len());
                });
                tx.consume_with(64, &mut |buffer| {
                    buffer.fill(0);
                    println!("Transmitted {} bytes", buffer.len());
                });
            }
        }

        debug!("Compatible object-safe devices test completed successfully");
    }
}
