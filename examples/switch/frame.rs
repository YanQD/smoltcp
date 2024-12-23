use spin::Mutex;
use std::sync::Arc;
use smoltcp::{
    time::Instant,
    phy::{Device, DeviceCapabilities, Medium, RxToken, TunTapInterface, TxToken}, 
};
use crate::switch::{PortReceiver, Producer};

// 帧捕获设备
pub struct FrameCapture {
    inner: Arc<Mutex<TunTapInterface>>,
    name: String,
    port_no: usize,
    // 发送通道：发送 (数据包, 源端口号) 到交换机
    frame_sender: Producer,
    // 接收通道：从交换机接收数据包
    frame_receiver: PortReceiver,
}

impl FrameCapture {
    pub fn new(
        name: &str,
        medium: Medium,
        frame_sender: Producer,
        frame_receiver: PortReceiver,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let inner = Arc::new(Mutex::new(TunTapInterface::new(name, medium)?));
        Ok(FrameCapture {
            inner,
            name: name.to_string(),
            port_no: 0,
            frame_sender,
            frame_receiver,
        })
    }

    pub fn set_port_no(&mut self, port_no: usize) {
        self.port_no = port_no;
    }
}

impl Device for FrameCapture {
    type RxToken<'a> = FrameCaptureRxToken;
    type TxToken<'a> = FrameCaptureTxToken<'a>;

    fn capabilities(&self) -> DeviceCapabilities {
        self.inner.lock().capabilities()
    }

    fn receive(&mut self, timestamp: Instant) -> Option<(Self::RxToken<'_>, Self::TxToken<'_>)> {
        if let Some(frame) = self.frame_receiver.try_recv() {
            return Some((
                FrameCaptureRxToken {
                    buffer: frame,
                    name: self.name.clone(),
                },
                FrameCaptureTxToken {
                    device: Arc::clone(&self.inner),
                    sender: &self.frame_sender,
                    port_no: self.port_no,
                    name: self.name.clone(),
                }
            ));
        }
    
        // 检查 tap 设备
        if let Some((rx, _)) = self.inner.lock().receive(timestamp) {
            return Some((
                FrameCaptureRxToken {
                    buffer: rx.consume(|buffer| buffer.to_vec()),
                    name: self.name.clone(),
                },
                FrameCaptureTxToken {
                    device: Arc::clone(&self.inner),
                    sender: &self.frame_sender,
                    port_no: self.port_no,
                    name: self.name.clone(),
                },
            ));
        }

        None
    }

    fn transmit(&mut self, _timestamp: Instant) -> Option<Self::TxToken<'_>> {
        Some(FrameCaptureTxToken {
            device: Arc::clone(&self.inner), // 克隆 Arc
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
    device: Arc<Mutex<TunTapInterface>>,
    sender: &'a Producer,
    port_no: usize,
    name: String,
}

impl<'a> TxToken for FrameCaptureTxToken<'a> {
    fn consume<R, F>(self, len: usize, f: F) -> R
    where
        F: FnOnce(&mut [u8]) -> R,
    {
        let mut buffer = vec![0u8; len];
        let result = f(&mut buffer);
        
        // 使用 device 发送数据
        if let Some(tx) = self.device.lock().transmit(Instant::now()) {
            tx.consume(len, |tx_buffer| {
                tx_buffer.copy_from_slice(&buffer);
            });
        }

        // 发送到交换机
        println!("\n[{}] Sending frame to switch from port {}", self.name, self.port_no);
        let _ = self.sender.try_push_switch_frame((buffer, self.port_no));
        
        result
    }
}