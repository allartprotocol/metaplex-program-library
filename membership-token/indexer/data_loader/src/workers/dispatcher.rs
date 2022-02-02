use parking_lot::Mutex;
use std::sync::Arc;

use super::{signatures_loader, transactions_loader};

use indexer_core::{Config, Storage};
use tokio::{
    sync::{
        broadcast::{self, Receiver, Sender},
        mpsc,
    },
    time::{sleep, Duration},
};

struct Connection<C, M> {
    _tx: Sender<C>,
    rx: Receiver<M>,
}

pub async fn run(config: &Config, mut stop_rx: Receiver<u8>, _stop_fb_tx: mpsc::Sender<()>) {
    println!("Dispatcher::run()");

    let (stop_tx, _stop_rx) = broadcast::channel::<u8>(32);

    // Feedback channel.
    // When every sender has gone out of scope, the recv call
    // will return with an error. This error allows us to know the moment when we could stop.
    let (stop_fb_tx, mut stop_fb_rx) = mpsc::channel::<()>(1);

    // The channels for communication with the workers
    let mut dispatcher_sgnloader_connection =
        setup_and_start_signatures_loader(config, stop_tx.clone(), stop_fb_tx.clone()).await;
    let mut dispatcher_trnsloaders_connection =
        setup_and_start_transactions_loaders(config, stop_tx.clone(), stop_fb_tx.clone()).await;

    // We will not send something via this channel
    drop(stop_fb_tx);

    loop {
        if let Ok(_message) = dispatcher_sgnloader_connection.rx.try_recv() {}
        if let Ok(_message) = dispatcher_trnsloaders_connection.rx.try_recv() {}

        sleep(Duration::from_millis(200)).await;

        if stop_rx.try_recv().is_ok() {
            break;
        }
    }

    stop_tx.send(0).unwrap();

    // When every sender has gone out of scope, the recv call will return with an error.
    let _ = stop_fb_rx.recv().await;

    println!("Dispatcher::stop()");
}

async fn setup_and_start_signatures_loader(
    config: &Config,
    stop_tx: broadcast::Sender<u8>,
    stop_fb_tx: mpsc::Sender<()>,
) -> Connection<signatures_loader::Command, signatures_loader::Message> {
    // The channel for sending messages from main to signatures_loader
    let (dispatcher_sgnloader_tx, dispatcher_sgnloader_rx) =
        broadcast::channel::<signatures_loader::Command>(32);

    // The channel for sending messages from signatures_loader to main
    let (sgnloader_dispatcher_tx, sgnloader_dispatcher_rx) =
        broadcast::channel::<signatures_loader::Message>(32);

    tokio::spawn(async move {
        super::signatures_loader::run(
            1,
            stop_tx.subscribe(),
            stop_fb_tx,
            sgnloader_dispatcher_tx,
            dispatcher_sgnloader_rx,
        )
        .await
    });

    let cmd = signatures_loader::Command::Start {
        config: config.clone(),
    };

    dispatcher_sgnloader_tx.send(cmd).unwrap();

    Connection {
        _tx: dispatcher_sgnloader_tx,
        rx: sgnloader_dispatcher_rx,
    }
}

async fn setup_and_start_transactions_loaders(
    config: &Config,
    stop_tx: Sender<u8>,
    stop_fb_tx: mpsc::Sender<()>,
) -> Connection<transactions_loader::Command, transactions_loader::Message> {
    // The channel for sending messages from main to signatures_loader
    let (dispatcher_trnsloader_tx, _dispatcher_trnsloader_rx) =
        broadcast::channel::<transactions_loader::Command>(32);

    // The channel for sending messages from signatures_loader to main
    let (trnsloader_dispatcher_tx, trnsloader_dispatcher_rx) =
        broadcast::channel::<transactions_loader::Message>(32);

    let storage = Storage::new(config.get_storage_config());
    let storage_guarded = Arc::new(Mutex::new(storage));

    let number_of_transaction_loaders = config
        .get_workers_pool_config()
        .nunmber_of_transaction_loaders;

    for channel_id in 0..number_of_transaction_loaders {
        let tx = trnsloader_dispatcher_tx.clone();
        let rx = dispatcher_trnsloader_tx.subscribe();
        let stp_tx = stop_tx.clone();
        let guarded_storage = Arc::clone(&storage_guarded);
        let stp_fb_tx = stop_fb_tx.clone();

        tokio::spawn(async move {
            super::transactions_loader::run(
                channel_id,
                stp_tx.subscribe(),
                stp_fb_tx,
                tx,
                rx,
                guarded_storage,
            )
            .await
        });

        let cmd = transactions_loader::Command::Start {
            channel_id,
            config: config.clone(),
        };

        dispatcher_trnsloader_tx.send(cmd).unwrap();
    }

    drop(stop_fb_tx);

    Connection {
        _tx: dispatcher_trnsloader_tx,
        rx: trnsloader_dispatcher_rx,
    }
}