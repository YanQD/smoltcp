use spin::Mutex;
use std::sync::Arc;
use smoltcp::{
    phy::{Device, DeviceCapabilities, RxToken, TxToken}, 
    time::Instant
};
use crate::{
    bridge_device::{boxed_object_safe_device, BridgeDevice, ObjectSafeDeviceOps}, 
    switch::{PortReceiver, Producer}
};

pub struct FrameCapture {
    inner: Arc<Mutex<BridgeDevice>>,
    name: String,
    port_no: usize,
    frame_sender: Producer,
    frame_receiver: PortReceiver,
}

impl FrameCapture {
    #[allow(unused)]
    pub fn new<D: Device + 'static>(
        name: &str,
        device: D,
        frame_sender: Producer,
        frame_receiver: PortReceiver,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let bridge_device = boxed_object_safe_device(device);

        Ok(FrameCapture {
            inner: Arc::new(Mutex::new(BridgeDevice::from_object_safe(bridge_device))),
            name: name.to_string(),
            port_no: 0,
            frame_sender,
            frame_receiver,
        })
    }

    #[allow(unused)]
    pub fn from_device(
        name: &str,
        device: Box<dyn ObjectSafeDeviceOps>,
        frame_sender: Producer,
        frame_receiver: PortReceiver,
    ) -> Self {
        FrameCapture {
            inner: Arc::new(Mutex::new(BridgeDevice::from_object_safe(device))),
            name: name.to_string(),
            port_no: 0,
            frame_sender,
            frame_receiver,
        }
    }

    pub fn set_port_no(&mut self, port_no: usize) {
        self.port_no = port_no;
    }
}

impl Device for FrameCapture {
    type RxToken<'a> = FrameCaptureRxToken where Self: 'a;
    type TxToken<'a> = FrameCaptureTxToken<'a> where Self: 'a;

    fn capabilities(&self) -> DeviceCapabilities {
        self.inner.lock().capabilities()
    }

    fn receive(&mut self, timestamp: Instant) -> Option<(Self::RxToken<'_>, Self::TxToken<'_>)> {
        println!("\n[{}] Checking for frames from switch", self.name);
        // First check for frames from switch
        if let Some(frame) = self.frame_receiver.try_recv() {
            println!("\n[{}] Received frame from switch, writing to device", self.name);
            
            return Some((
                FrameCaptureRxToken {
                    buffer: frame,
                    name: self.name.clone(),
                },
                FrameCaptureTxToken {
                    device: Arc::clone(&self.inner),
                    buffer: Vec::new(),
                    max_len: 0,
                    sender: &self.frame_sender,
                    port_no: self.port_no,
                    name: self.name.clone(),
                }
            ));
        }
        
        println!("[{}] No frames from switch", self.name);

        // Then check the underlying device
        let mut inner = self.inner.lock();
        let (mut rx, _tx) = inner.receive(timestamp)?;
        
        // Immediately consume the rx token to get the data
        let mut rx_buffer = Vec::new();
        rx.rx.consume_with(&mut |data| {
            rx_buffer.extend_from_slice(data);
        });
        
        Some((
            FrameCaptureRxToken {
                buffer: rx_buffer,
                name: self.name.clone(),
            },
            FrameCaptureTxToken {
                device: Arc::clone(&self.inner),
                buffer: Vec::new(),
                max_len: 0,
                sender: &self.frame_sender,
                port_no: self.port_no,
                name: self.name.clone(),
            }
        ))
    }

    fn transmit(&mut self, _timestamp: Instant) -> Option<Self::TxToken<'_>> {
        Some(FrameCaptureTxToken {
            device: Arc::clone(&self.inner), // 克隆 Arc
                buffer: Vec::new(),
                max_len: 0,
                sender: &self.frame_sender,
                port_no: self.port_no,
                name: self.name.clone(),
        })
    }
}

// 接收Token
pub struct FrameCaptureRxToken {
    buffer: Vec<u8>,
    name: String,
}

impl RxToken for FrameCaptureRxToken {
    fn consume<R, F>(mut self, f: F) -> R
    where
        F: FnOnce(&mut [u8]) -> R,
    {
        println!("\n[{}] Processing received frame of size: {}", self.name, self.buffer.len());
        f(&mut self.buffer)
    }
}

// 发送Token
pub struct FrameCaptureTxToken<'a> {
    device: Arc<Mutex<BridgeDevice>>,
    buffer: Vec<u8>,
    max_len: usize,
    sender: &'a Producer,
    port_no: usize,
    name: String,
}

impl<'a> TxToken for FrameCaptureTxToken<'a> {
    fn consume<R, F>(mut self, len: usize, f: F) -> R
    where
        F: FnOnce(&mut [u8]) -> R,
    {
        self.max_len = len;
        self.buffer.resize(len, 0);
        
        let result = f(&mut self.buffer);

        if let Some(tx) = self.device.lock().transmit(Instant::now()) {
            tx.consume(len, |tx_buffer| {
                tx_buffer.copy_from_slice(&self.buffer);
            });
        }
        
        println!("\n[{}] Sending frame to switch from port {}", self.name, self.port_no);
        let _ = self.sender.try_push_switch_frame((self.buffer.clone(), self.port_no));
        
        result
    }
}
