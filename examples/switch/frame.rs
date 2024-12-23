use std::sync::Arc;
use smoltcp::{phy::{Device, DeviceCapabilities, Medium, RxToken, TunTapInterface, TxToken}, time::Instant};
use crate::switch::{PortReceiver, Producer};

// 帧捕获设备
pub struct FrameCapture {
    inner: Arc<spin::Mutex<TunTapInterface>>,
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
        let inner = Arc::new(spin::Mutex::new(TunTapInterface::new(name, medium)?));
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
        println!("\n[{}] Checking for frames from switch", self.name);
        // 优先检查是否有来自交换机的数据
        if let Some(frame) = self.frame_receiver.try_recv() {
            println!("frame received from switch!");
            println!("\n[{}] Received frame from switch, writing to tap", self.name);
            // 直接写入到tap设备
            return Some((
                FrameCaptureRxToken {
                    buffer: frame,
                    name: self.name.clone(),
                },
                FrameCaptureTxToken {
                    inner: self.inner.lock().transmit(timestamp).unwrap(),
                    sender: &self.frame_sender,
                    port_no: self.port_no,
                    name: self.name.clone(),
                }
            ));
        } else {
            println!("[{}] No frames from switch", self.name);
        }
    
        // 然后检查tap设备是否有新数据
        let result = self.inner.lock().receive(timestamp).map(|(rx, tx)| {
            (
                FrameCaptureRxToken {
                    buffer: rx.consume(|buffer| buffer.to_vec()),
                    name: self.name.clone(),
                },
                FrameCaptureTxToken {
                    inner: tx,
                    sender: &self.frame_sender,
                    port_no: self.port_no,
                    name: self.name.clone(),
                },
            )
        });

        if result.is_none() {
            println!("[{}] No frames from tap device", self.name);
        }

        result
    }

    fn transmit(&mut self, timestamp: Instant) -> Option<Self::TxToken<'_>> {
        self.inner.lock().transmit(timestamp).map(|tx| {
            FrameCaptureTxToken {
                inner: tx,
                sender: &self.frame_sender,
                port_no: self.port_no,
                name: self.name.clone(),
            }
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
    inner: <TunTapInterface as Device>::TxToken<'a>,
    sender: &'a Producer,
    port_no: usize,
    name: String,
}

impl<'a> TxToken for FrameCaptureTxToken<'a> {
    fn consume<R, F>(self, len: usize, f: F) -> R
    where
        F: FnOnce(&mut [u8]) -> R,
    {
        self.inner.consume(len, |buffer| {
            let result = f(buffer);
            println!("FrameCaptureTxToken consume: {:?}", buffer);
            println!("\n[{}] Sending frame to switch from port {}", self.name, self.port_no);
            let _ = self.sender.try_push_switch_frame((buffer.to_vec(), self.port_no));
            result
        })
    }
}