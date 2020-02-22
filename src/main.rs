#[macro_use] extern crate log;
#[macro_use] extern crate error_chain;
extern crate colored;
extern crate tungstenite;
extern crate workerpool;

use std::net::TcpListener;
use std::sync::mpsc::{self, Sender, Receiver};
use std::collections::HashMap;
use log::{Level, LevelFilter, Metadata, Record};
use colored::Colorize;
use workerpool::Pool;
use workerpool::thunk::{Thunk, ThunkWorker};
use tungstenite::server::accept as accept_stream;

mod errors {
  error_chain! {}
}
use errors::*;

struct OurLogger;
impl log::Log for OurLogger {
  fn enabled(&self, _: &Metadata) -> bool {
    true
  }
  fn log(&self, rec: &Record) {
    if self.enabled(rec.metadata()) {
      match rec.level() {
        Level::Error => eprintln!("{} {}", "error:".red()         .bold(), rec.args()),
        Level::Warn  => eprintln!("{} {}", "warn:" .yellow()      .bold(), rec.args()),
        Level::Info  => eprintln!("{} {}", "info:" .yellow()      .bold(), rec.args()),
        Level::Debug => eprintln!("{} {}", "debug:".bright_black().bold(), rec.args()),
        Level::Trace => eprintln!("{} {}", "trace:".blue()        .bold(), rec.args()),
      }
    }
  }
  fn flush(&self) {}
}

pub enum Message {
  Text(u64, u64, String),
}

mod action {
  use Message;
  use std::sync::mpsc::Sender;
  use std::net::TcpStream;

  pub enum Server {
    Listen(u16),
    Log(String),
    Connect(u64, TcpStream),
    CreateRoom(u64),
    Send(Message),
  }
  pub enum Room {
    Send(Message),
    Join(u64, Sender<Message>),
  }
}

fn create_room(
  room_id: u64,
  worker_pool: &Pool::<ThunkWorker<action::Server>>,
  _server_send: Sender<action::Server>,
  room_receive: Receiver<action::Room>,
) {
  worker_pool.execute(Thunk::of(move || {
    let mut users = HashMap::new();
    while let Some(action) = room_receive.recv().ok() {
      match action {
        action::Room::Send(message) => {
          let Message::Text(from, _to, text) = message;
          users.iter().filter(|(id, _send): &(&u64, &Sender<Message>)| **id != from).for_each(|(id, send)| {
            debug!("sending message through worker #{}: '{}'", id, text.yellow());
            send
              .send(Message::Text(from, room_id, text.clone()))
              .unwrap_or_else(|_err| error!("cannot send to #{}", id))
          })
        }
        action::Room::Join(id, user_send) => { users.insert(id, user_send); },
      }
    }
    action::Server::Log(format!("Room #{} closed", room_id))
  }));
}

fn run() -> Result<()> {
  const ROOM_ID: u64 = 1;

  let (server_send, server_receive) = mpsc::channel();
  let worker_pool = Pool::<ThunkWorker<action::Server>>::new(16);

  let mut users: HashMap<u64, Sender<Message>> = HashMap::new();
  let mut rooms: HashMap<u64, Sender<action::Room>> = HashMap::new();

  server_send.send(action::Server::Listen(1337)).unwrap();
  server_send.send(action::Server::CreateRoom(1)).unwrap();

  while let Some(act) = server_receive.recv().ok() {
    match act {
      action::Server::Listen(port) => {
        let server_send = server_send.clone();
        worker_pool.execute(Thunk::of(move || {
          let listener = TcpListener::bind(format!("127.0.0.1:{}", port)).unwrap();
          let mut next_user_id = 0u64;
          for (_, stream) in listener.incoming().enumerate() {
            next_user_id += 1;
            server_send.send(action::Server::Connect(next_user_id, stream.unwrap())).unwrap();
          }
          action::Server::Listen(port)
        }));
        info!("Listening started on port {}", port);
      }

      action::Server::Log(message) => { info!("{}", message); }

      action::Server::Connect(id, stream) => {
        let mut websocket = accept_stream(stream).unwrap();
        let (user_send, user_receive) = mpsc::channel();
        let server_send = server_send.clone();
        worker_pool.execute(Thunk::of(move || {
          loop {
            let message = websocket.read_message().unwrap().to_string();
            debug!("reader #{} received '{}'", id, message.yellow());
            server_send.send(action::Server::Send(Message::Text(id, ROOM_ID, message))).unwrap();
          }
        }));
        worker_pool.execute(Thunk::of(move || {
          while let Some(message) = user_receive.recv().ok() {
            match message {
              Message::Text(_from, _to, message) => {
                // websocket.write_message(tungstenite::Message::Text(message.clone())).unwrap();
              }
            }
          }
          action::Server::Log(format!("User #{} output thread terminating", id))
        }));
        users.insert(id, user_send.clone());
        info!("User #{} connected to the server", id);

        rooms[&ROOM_ID].send(action::Room::Join(id, user_send)).unwrap();
      }

      action::Server::CreateRoom(id) => {
        let (room_send, room_receive) = mpsc::channel();
        create_room(id, &worker_pool, server_send.clone(), room_receive);
        rooms.insert(id, room_send);
        info!("Room #{} created", id);
      }

      action::Server::Send(message) => {
        match message {
          Message::Text(_, room_id, _) => {
            rooms[&room_id].send(action::Room::Send(message)).unwrap();
          }
        }
      }
    }
  }
  Ok(())
}

fn main() {
  log::set_logger(&OurLogger).unwrap();
  log::set_max_level(LevelFilter::Trace);

  if let Err(ref e) = run() {
    error!("{}", e);
    for e in e.iter().skip(1) {
      error!("{} {}", "caused by:".bright_black().bold(), e);
    }

    // Use `RUST_BACKTRACE=1` to enable the backtraces.
    if let Some(backtrace) = e.backtrace() {
      error!("{} {:?}", "backtrace:".blue().bold(), backtrace);
    }
    std::process::exit(1);
  }
}
