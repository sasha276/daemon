pub mod stream;

use std::net::SocketAddr;
use std::sync::Arc;
use tokio::net::UdpSocket;

use device_lib::{
    DeviceManager, CanFrame, CanInterface,
    BaudRate, UartMode, GenCode, GenSettings,
};

use crate::client::ClientRegistry;
use crate::error::AppError;
use crate::protocol::{Cmd, Packet, read_u8, read_u16, read_u32};
use crate::session::SessionManager;
use crate::transport::stream::StreamManager;

pub struct Server {
    socket:   Arc<UdpSocket>,
    clients:  Arc<ClientRegistry>,
    sessions: Arc<SessionManager>,
    devices:  Arc<DeviceManager>,
    streams:  Arc<StreamManager>,
}

impl Server {
    pub async fn bind(addr: &str) -> Result<Self, AppError> {
        let socket = Arc::new(UdpSocket::bind(addr).await?);
        tracing::info!("UDP server bound to {addr}");
        Ok(Self {
            socket,
            clients:  Arc::new(ClientRegistry::new()),
            sessions: Arc::new(SessionManager::new()),
            devices:  DeviceManager::new(),
            streams:  StreamManager::new(),
        })
    }

    pub async fn run(self) -> Result<(), AppError> {
        self.devices.refresh().await;

        let socket   = self.socket.clone();
        let clients  = self.clients.clone();
        let sessions = self.sessions.clone();
        let devices  = self.devices.clone();
        let streams  = self.streams.clone();

        let mut buf = vec![0u8; 65535];
        tracing::info!("server ready — waiting for packets");

        loop {
            let (len, addr) = socket.recv_from(&mut buf).await?;
            let data = buf[..len].to_vec();

            let sock     = socket.clone();
            let clients  = clients.clone();
            let sessions = sessions.clone();
            let devices  = devices.clone();
            let streams  = streams.clone();

            tokio::spawn(async move {
                if let Err(e) = handle_packet(
                    sock, addr, data, clients, sessions, devices, streams
                ).await {
                    tracing::warn!("packet error from {addr}: {e}");
                }
            });
        }
    }
}

/// Разбирает payload CAN-команды записи в `(интерфейс, кадр)`.
///
/// Формат payload: `[iface:u8][id:u16 BE][data: 0..=8 байт]`.
fn parse_can_payload(payload: &[u8]) -> Result<(CanInterface, CanFrame), AppError> {
    if payload.len() < 3 {
        return Err(AppError::PayloadDecode("CAN payload too short".into()));
    }
    let iface = CanInterface::from_u8(payload[0])
        .ok_or_else(|| AppError::PayloadDecode(format!("bad CAN interface {}", payload[0])))?;
    let id = u16::from_be_bytes([payload[1], payload[2]]);
    let data = &payload[3..];
    let frame = CanFrame::new(id, data);
    Ok((iface, frame))
}

/// Разбирает payload команды GenSetMode в [`GenSettings`].
///
/// Layout payload (big-endian для многобайтовых полей):
///   `[code:u8][gain:u8][dac:u16][alsn:u8][freq:u32][alsen:u16][saut_freq:u8][saut_blocks:6]`
fn parse_gen_settings(payload: &[u8]) -> Result<GenSettings, AppError> {
    if payload.len() < 18 {
        return Err(AppError::PayloadDecode("gen payload too short".into()));
    }
    let code = GenCode::from_u8(payload[0])
        .ok_or_else(|| AppError::PayloadDecode(format!("bad gen code {}", payload[0])))?;
    let gain_x10 = payload[1] != 0;
    let dac = u16::from_be_bytes([payload[2], payload[3]]);
    let alsn_code = payload[4];
    let frequency = u32::from_be_bytes([payload[5], payload[6], payload[7], payload[8]]);
    let alsen_sg_kk = u16::from_be_bytes([payload[9], payload[10]]);
    let saut_freq = payload[11];
    let mut saut_blocks = [0u8; 6];
    saut_blocks.copy_from_slice(&payload[12..18]);
    Ok(GenSettings {
        code,
        gain_x10,
        dac,
        alsn_code,
        frequency,
        alsen_sg_kk,
        saut_freq,
        saut_blocks,
    })
}

async fn handle_packet(
    socket:   Arc<UdpSocket>,
    addr:     SocketAddr,
    data:     Vec<u8>,
    clients:  Arc<ClientRegistry>,
    sessions: Arc<SessionManager>,
    devices:  Arc<DeviceManager>,
    streams:  Arc<StreamManager>,
) -> Result<(), AppError> {
    // Если не удалось распарсить даже заголовок — ответить нечем (нет seq/cid),
    // просто логируем выше по стеку.
    let packet = Packet::parse(&data)?;

    if packet.is_response() {
        return Ok(());
    }

    // Любая ошибка обработки уже распарсенного пакета должна вернуться клиенту
    // как Error-пакет, иначе клиент получит timeout вместо причины.
    let resp = match process(
        addr, &packet, &clients, &sessions, &devices, &streams,
    )
    .await
    {
        Ok(resp) => resp,
        Err(e) => Packet::error(&packet, &e.to_string()),
    };

    socket.send_to(&resp, addr).await?;
    Ok(())
}

async fn process(
    addr:     SocketAddr,
    packet:   &Packet,
    clients:  &Arc<ClientRegistry>,
    sessions: &Arc<SessionManager>,
    devices:  &Arc<DeviceManager>,
    streams:  &Arc<StreamManager>,
) -> Result<Vec<u8>, AppError> {
    let cmd = packet.cmd()?;

    if cmd != Cmd::Register && clients.get_by_addr(&addr).is_none() {
        return Ok(Packet::error(packet, "not registered — send Register first"));
    }

    let resp: Vec<u8> = match cmd {

        // ── client ────────────────────────────────────────────────────────

        Cmd::Register => {
            let id = clients.register(addr);
            Packet::response(packet, &id.to_be_bytes())
        }

        Cmd::Unregister => {
            let id = packet.client_id;
            streams.close_all_for_client(id).await;
            sessions.close_all_for_client(id);
            clients.unregister(id);
            Packet::response(packet, &[])
        }

        Cmd::Ping => Packet::response(packet, b"pong"),

        // ── devices ───────────────────────────────────────────────────────

        Cmd::DeviceList => {
            devices.refresh().await;
            let list = devices.list_info().await;
            let mut p: Vec<u8> = Vec::new();
            p.extend_from_slice(&(list.len() as u16).to_be_bytes());
            for (i, dev) in list.iter().enumerate() {
                p.extend_from_slice(&(i as u16).to_be_bytes());
                p.extend_from_slice(&dev.id.vid.to_be_bytes());
                p.extend_from_slice(&dev.id.pid.to_be_bytes());
                p.push(dev.id.bus);
                let port_b = dev.id.port.as_bytes();
                p.push(port_b.len() as u8);
                p.extend_from_slice(port_b);
                let mfr = dev.manufacturer.as_bytes();
                p.push(mfr.len() as u8);
                p.extend_from_slice(mfr);
                let prod = dev.product.as_bytes();
                p.push(prod.len() as u8);
                p.extend_from_slice(prod);
            }
            Packet::response(packet, &p)
        }

        Cmd::DeviceInfo => {
            let idx = read_u16(&packet.payload, 0)? as usize;
            match devices.get_info(idx).await {
                Some(dev) => {
                    let mut p = Vec::new();
                    p.extend_from_slice(&dev.id.vid.to_be_bytes());
                    p.extend_from_slice(&dev.id.pid.to_be_bytes());
                    p.push(dev.id.bus);
                    let port_b = dev.id.port.as_bytes();
                    p.push(port_b.len() as u8);
                    p.extend_from_slice(port_b);
                    Packet::response(packet, &p)
                }
                None => Packet::error(packet, &format!("no device at index {idx}")),
            }
        }

        // ── sessions ──────────────────────────────────────────────────────

        Cmd::SessionCreate => {
            let idx = read_u16(&packet.payload, 0)? as usize;
            match devices.get_info(idx).await {
                Some(dev) => {
                    let sid = sessions.create(packet.client_id, idx, dev.id.key())?;
                    Packet::response(packet, &sid.to_be_bytes())
                }
                None => Packet::error(packet, &format!("no device at index {idx}")),
            }
        }

        Cmd::SessionClose => {
            let sid = read_u16(&packet.payload, 0)?;
            streams.close(packet.client_id, sid).await;
            match sessions.close(sid) {
                Ok(())  => Packet::response(packet, &[]),
                Err(e)  => Packet::error(packet, &e.to_string()),
            }
        }

        Cmd::SessionList => {
            let list = sessions.list_for_client(packet.client_id);
            let mut p = Vec::new();
            p.extend_from_slice(&(list.len() as u16).to_be_bytes());
            for s in &list {
                p.extend_from_slice(&s.id.to_be_bytes());
                p.extend_from_slice(&(s.device_idx as u16).to_be_bytes());
                let key_b = s.device_key.as_bytes();
                p.push(key_b.len() as u8);
                p.extend_from_slice(key_b);
            }
            Packet::response(packet, &p)
        }

        // device commands

        Cmd::GetVersion => {

            tracing::info!("Проверка дохода в метод Cmd::GetVersion");

            let sess = sessions.get(packet.session_id)?;

            tracing::info!("Проверка после получеиня сессии");

            match devices.get_driver(sess.device_idx).await {
                Ok(d) => match d.get_version().await {
                    Ok(ver) => Packet::response(packet, ver.as_bytes()),
                    Err(e)  => Packet::error(packet, &e.to_string()),
                },
                Err(e) => Packet::error(packet, &e.to_string()),
            }
        }

        Cmd::CheckUart => {
            let sess = sessions.get(packet.session_id)?;
            match devices.get_driver(sess.device_idx).await {
                Ok(d) => {
                    let has = d.capabilities().uart;
                    Packet::response(packet, &[has as u8])
                }
                Err(e) => Packet::error(packet, &e.to_string()),
            }
        }

        // ── CAN одиночная запись ──────────────────────────────────────────

        Cmd::CanWrite => {
            // payload: [iface:u8][id:u16 BE][data...]
            let sess = sessions.get(packet.session_id)?;
            match devices.get_driver(sess.device_idx).await {
                Ok(d) => match d.as_can() {
                    Some(can) => match parse_can_payload(&packet.payload) {
                        Ok((iface, frame)) => match can.can_write(iface, &frame).await {
                            Ok(())  => Packet::response(packet, &[]),
                            Err(e)  => Packet::error(packet, &e.to_string()),
                        },
                        Err(e) => Packet::error(packet, &e.to_string()),
                    },
                    None => Packet::error(packet, "device has no CAN capability"),
                },
                Err(e) => Packet::error(packet, &e.to_string()),
            }
        }

        // ── CAN периодическая запись ──────────────────────────────────────

        Cmd::CanPeriodicStart => {
            // payload: interval_ms:u16 + [iface:u8][id:u16 BE][data...]
            let interval_ms = read_u16(&packet.payload, 0)?;
            let frame_payload = packet.payload[2..].to_vec();
            let (iface, frame) = parse_can_payload(&frame_payload)?;

            let sess = sessions.get(packet.session_id)?;
            match devices.get_driver(sess.device_idx).await {
                Ok(driver) => {
                    if driver.as_can().is_none() {
                        Packet::error(packet, "device has no CAN capability")
                    } else {
                        let (stop_tx, mut stop_rx) = tokio::sync::oneshot::channel::<()>();
                        streams.add_periodic(packet.client_id, packet.session_id, stop_tx).await;

                        tokio::spawn(async move {
                            let mut ticker = tokio::time::interval(
                                std::time::Duration::from_millis(interval_ms as u64)
                            );
                            loop {
                                tokio::select! {
                                    _ = ticker.tick() => {
                                        if let Some(can) = driver.as_can() {
                                            if let Err(e) = can.can_write(iface, &frame).await {
                                                tracing::warn!("periodic can_write error: {e}");
                                                break;
                                            }
                                        } else {
                                            break;
                                        }
                                    }
                                    _ = &mut stop_rx => break,
                                }
                            }
                            tracing::debug!("periodic task stopped");
                        });

                        Packet::response(packet, &[])
                    }
                }
                Err(e) => Packet::error(packet, &e.to_string()),
            }
        }

        Cmd::CanPeriodicStop => {
            streams.stop_periodic(packet.client_id, packet.session_id).await;
            Packet::response(packet, &[])
        }

        // ── CAN настройка скорости ────────────────────────────────────────

        Cmd::CanSetBaudrate => {
            // payload: iface:u8 + baud:u16
            let iface_n = read_u8(&packet.payload, 0)?;
            let baud_raw = read_u16(&packet.payload, 1)?;
            let sess = sessions.get(packet.session_id)?;
            match (CanInterface::from_u8(iface_n), devices.get_driver(sess.device_idx).await) {
                (Some(iface), Ok(d)) => match d.as_can() {
                    Some(can) => match can
                        .set_baudrate(iface, BaudRate::from_raw(baud_raw))
                        .await
                    {
                        Ok(())  => Packet::response(packet, &[]),
                        Err(e)  => Packet::error(packet, &e.to_string()),
                    },
                    None => Packet::error(packet, "device has no CAN capability"),
                },
                (None, _) => Packet::error(packet, &format!("bad CAN interface {iface_n}")),
                (_, Err(e)) => Packet::error(packet, &e.to_string()),
            }
        }

        // ── CAN одиночное чтение ──────────────────────────────────────────

        Cmd::CanReadOnce => {
            // payload: iface:u8 ; ответ: count:u16 + повтор [len:u8][hex bytes]
            let iface_n = read_u8(&packet.payload, 0)?;
            let sess = sessions.get(packet.session_id)?;
            match (CanInterface::from_u8(iface_n), devices.get_driver(sess.device_idx).await) {
                (Some(iface), Ok(d)) => match d.as_can() {
                    Some(can) => match can.can_read(iface).await {
                        Ok(frames) => {
                            let mut p = Vec::new();
                            p.extend_from_slice(&(frames.len() as u16).to_be_bytes());
                            for f in &frames {
                                let line = f.to_hex_line();
                                let b = line.as_bytes();
                                p.push(b.len() as u8);
                                p.extend_from_slice(b);
                            }
                            Packet::response(packet, &p)
                        }
                        Err(e) => Packet::error(packet, &e.to_string()),
                    },
                    None => Packet::error(packet, "device has no CAN capability"),
                },
                (None, _) => Packet::error(packet, &format!("bad CAN interface {iface_n}")),
                (_, Err(e)) => Packet::error(packet, &e.to_string()),
            }
        }

        // ── UART ──────────────────────────────────────────────────────────

        Cmd::UartConfigure => {
            // payload: mode:u8
            let mode_n = read_u8(&packet.payload, 0)?;
            let sess = sessions.get(packet.session_id)?;
            match (UartMode::from_u8(mode_n), devices.get_driver(sess.device_idx).await) {
                (Some(mode), Ok(d)) => match d.as_uart() {
                    Some(uart) => match uart.uart_configure(mode).await {
                        Ok(())  => Packet::response(packet, &[]),
                        Err(e)  => Packet::error(packet, &e.to_string()),
                    },
                    None => Packet::error(packet, "device has no UART capability"),
                },
                (None, _) => Packet::error(packet, &format!("bad UART mode {mode_n}")),
                (_, Err(e)) => Packet::error(packet, &e.to_string()),
            }
        }

        Cmd::UartWrite => {
            // payload: data bytes
            let sess = sessions.get(packet.session_id)?;
            match devices.get_driver(sess.device_idx).await {
                Ok(d) => match d.as_uart() {
                    Some(uart) => match uart.uart_write(&packet.payload).await {
                        Ok(())  => Packet::response(packet, &[]),
                        Err(e)  => Packet::error(packet, &e.to_string()),
                    },
                    None => Packet::error(packet, "device has no UART capability"),
                },
                Err(e) => Packet::error(packet, &e.to_string()),
            }
        }

        Cmd::UartRead => {
            let sess = sessions.get(packet.session_id)?;
            match devices.get_driver(sess.device_idx).await {
                Ok(d) => match d.as_uart() {
                    Some(uart) => match uart.uart_read().await {
                        Ok(data) => Packet::response(packet, &data),
                        Err(e)   => Packet::error(packet, &e.to_string()),
                    },
                    None => Packet::error(packet, "device has no UART capability"),
                },
                Err(e) => Packet::error(packet, &e.to_string()),
            }
        }

        Cmd::UartStreamOpen | Cmd::UartStreamClose => {
            // TODO (Этап 5): UART-стрим по аналогии с CAN-стримом
            Packet::error(packet, "UART stream not yet implemented")
        }

        // ── генератор ─────────────────────────────────────────────────────

        Cmd::GenSetMode => {
            let sess = sessions.get(packet.session_id)?;
            match devices.get_driver(sess.device_idx).await {
                Ok(d) => match d.as_gen() {
                    Some(gen) => match parse_gen_settings(&packet.payload) {
                        Ok(settings) => match gen.gen_set(&settings).await {
                            Ok(())  => Packet::response(packet, &[]),
                            Err(e)  => Packet::error(packet, &e.to_string()),
                        },
                        Err(e) => Packet::error(packet, &e.to_string()),
                    },
                    None => Packet::error(packet, "device has no generator capability"),
                },
                Err(e) => Packet::error(packet, &e.to_string()),
            }
        }

        Cmd::GenSetFrequency => {
            // payload: channel:u8 + frequency:u32
            let channel = read_u8(&packet.payload, 0)?;
            let freq    = read_u32(&packet.payload, 1)?;
            let sess = sessions.get(packet.session_id)?;
            match devices.get_driver(sess.device_idx).await {
                Ok(d) => match d.as_gen() {
                    Some(gen) => match gen.gen_set_frequency(channel, freq).await {
                        Ok(())  => Packet::response(packet, &[]),
                        Err(e)  => Packet::error(packet, &e.to_string()),
                    },
                    None => Packet::error(packet, "device has no generator capability"),
                },
                Err(e) => Packet::error(packet, &e.to_string()),
            }
        }

        Cmd::GenSetAmplitude => {
            // payload: channel:u8 + amplitude:u16
            let channel = read_u8(&packet.payload, 0)?;
            let ampl    = read_u16(&packet.payload, 1)?;
            let sess = sessions.get(packet.session_id)?;
            match devices.get_driver(sess.device_idx).await {
                Ok(d) => match d.as_gen() {
                    Some(gen) => match gen.gen_set_amplitude(channel, ampl).await {
                        Ok(())  => Packet::response(packet, &[]),
                        Err(e)  => Packet::error(packet, &e.to_string()),
                    },
                    None => Packet::error(packet, "device has no generator capability"),
                },
                Err(e) => Packet::error(packet, &e.to_string()),
            }
        }

        // ── CAN стрим (чтение потоком) ────────────────────────────────────

        Cmd::StreamOpen => {
            // payload: client_data_port:u16  serial_number:u16
            let client_data_port = read_u16(&packet.payload, 0)?;
            let serial_number    = read_u16(&packet.payload, 2)? as u8;
            let data_addr        = SocketAddr::new(addr.ip(), client_data_port);

            let iface = match CanInterface::from_u8(serial_number) {
                Some(i) => i,
                None => {
                    return Ok(Packet::error(
                        packet,
                        &format!("bad CAN interface {serial_number}"),
                    ));
                }
            };

            let sess = sessions.get(packet.session_id)?;
            match devices.get_driver(sess.device_idx).await {
                Ok(driver) => {
                    if driver.as_can().is_none() {
                        Packet::error(packet, "device has no CAN capability")
                    } else {
                        match streams.open(
                            packet.client_id, packet.session_id, data_addr
                        ).await {
                            Ok(local_port) => {
                                let streams_clone = streams.clone();
                                let client_id     = packet.client_id;
                                let session_id    = packet.session_id;

                                // Задача: читаем кадры с устройства, форматируем в hex,
                                // пушим клиенту по одной строке на кадр.
                                tokio::spawn(async move {
                                    loop {
                                        let can = match driver.as_can() {
                                            Some(c) => c,
                                            None => break,
                                        };
                                        match can.can_read(iface).await {
                                            Ok(frames) => {
                                                for frame in frames {
                                                    let line = frame.to_hex_line();
                                                    streams_clone
                                                        .push_frame(
                                                            client_id,
                                                            session_id,
                                                            line.as_bytes(),
                                                        )
                                                        .await;
                                                }
                                            }
                                            Err(e) => {
                                                tracing::warn!("can_read error: {e}");
                                                break;
                                            }
                                        }
                                    }
                                });

                                Packet::response(packet, &local_port.to_be_bytes())
                            }
                            Err(e) => Packet::error(packet, &e.to_string()),
                        }
                    }
                }
                Err(e) => Packet::error(packet, &e.to_string()),
            }
        }

        Cmd::StreamClose => {
            streams.close(packet.client_id, packet.session_id).await;
            Packet::response(packet, &[])
        }

        // ── серверные команды (клиент не должен слать) ────────────────────

        Cmd::CanFrame | Cmd::Event => {
            Packet::error(packet, "server-only command")
        }

        // Клиент прислал error/response-пакет — отвечаем пустым, клиент отбросит.
        Cmd::Error => Packet::response(packet, &[]),
    };

    Ok(resp)
}
