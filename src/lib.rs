mod redislogic;

use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;
use std::thread;

use druid::im::{vector, Vector};
use druid::widget::{
    Align, Button, Controller, Either, Flex, Label, List, ListIter, Padding, Scroll, TabsPolicy,
    TextBox, ViewSwitcher,
};
use druid::{
    lens, AppLauncher, BoxConstraints, Color, Data, Env, Event, EventCtx, LayoutCtx, Lens, LensExt,
    LifeCycle, LifeCycleCtx, LocalizedString, PaintCtx, PlatformError, Size, UnitPoint, UpdateCtx,
    Widget, WidgetExt, WindowDesc,
};
use redis::Connection;
use redislogic::redislogic::{
    connect_redis, convert_keys_to_namespaces, delete_redis_key, get_all_keys, get_redis_value,
    set_redis_value, RedisNamespace, RedisValue,
};

pub fn run_app() -> Result<(), PlatformError> {
    let window = WindowDesc::new(build_ui())
        .window_size((223., 300.))
        .resizable(true)
        .title(LocalizedString::new("redis-viewer-window-title").with_placeholder("Redis Viewer"));

    let launcher = AppLauncher::with_window(window).log_to_console();

    let event_sink = launcher.get_external_handle();
    let (sender, receiver) = channel::<RedisViewerEvent>();
    thread::spawn(move || handle_events(event_sink, receiver));

    let keys = Vec::<String>::new();
    let redis_viewer_state = RedisViewerState {
        sender: Arc::from(sender),
        keys: Vector::from(keys),
        keys_senders: Vector::from(Vec::<ItemSender>::new()),
        is_refreshing: false,
        is_connection_form_showing: true,
        connection_address: Arc::from("127.0.0.1".to_string()),
        connection_port: Arc::from("6379".to_string()),
        connection_db: Arc::from("0".to_string()),
        redis_value: Arc::from(None),
    };

    launcher.launch(redis_viewer_state)?;

    Ok(())
}

enum RedisViewerEvent {
    RefreshKeys,
    CreateConnection(String, String, String),
    SelectRedisValue(String),
}

fn handle_events(event_sink: druid::ExtEventSink, receiver: Receiver<RedisViewerEvent>) {
    let mut redis: Option<Connection> = None;

    loop {
        match receiver.try_recv() {
            Ok(event) => match event {
                RedisViewerEvent::RefreshKeys => {
                    match redis {
                        Some(ref mut connection) => {
                            let keys = get_all_keys(connection).expect("failed to get keys");
                            sync_keys(&event_sink, keys);
                        }
                        None => {
                            event_sink.add_idle_callback(move |data: &mut RedisViewerState| {
                                data.keys = Vector::from(Vec::<String>::new());
                                data.is_refreshing = false;
                            });
                        }
                    };
                }
                RedisViewerEvent::CreateConnection(address, port, db) => {
                    let port: u16 = port.parse().expect("failed to parse port");
                    let db: i64 = db.parse().expect("failed to parse db");
                    let connection = connect_redis(&address, port, db);
                    match connection {
                        Ok(mut conn) => {
                            event_sink.add_idle_callback(move |data: &mut RedisViewerState| {
                                data.is_connection_form_showing = false;
                                data.is_refreshing = true;
                            });
                            let keys = get_all_keys(&mut conn).expect("failed to get keys");
                            sync_keys(&event_sink, keys);
                            redis = Some(conn);
                        }
                        Err(_err) => {
                            event_sink.add_idle_callback(move |data: &mut RedisViewerState| {
                                data.is_connection_form_showing = true;
                                println!("failed to connect to redis");
                            });
                        }
                    };
                }
                RedisViewerEvent::SelectRedisValue(key) => {
                    match redis {
                        Some(ref mut connection) => {
                            let redis_value = get_redis_value(connection, &key)
                                .expect("failed to get value for key");
                            match redis_value {
                                RedisValue::String(v) => println!("{}", v),
                                RedisValue::List(_) => (),
                                RedisValue::Set(_) => (),
                                RedisValue::ZSet(_) => (),
                                RedisValue::Hash(_) => (),
                                RedisValue::Null => (),
                            }
                            let redis_value = get_redis_value(connection, &key)
                                .expect("failed to get value for key");
                            event_sink.add_idle_callback(move |data: &mut RedisViewerState| {
                                data.redis_value = Arc::from(Some(redis_value));
                            });
                        }
                        None => {
                            println!("no connection");
                            event_sink.add_idle_callback(move |data: &mut RedisViewerState| {
                                data.redis_value = Arc::from(None);
                            });
                        }
                    };
                }
            },
            Err(e) => {
                if e == std::sync::mpsc::TryRecvError::Disconnected {
                    break;
                }
            }
        }
    }
}

fn sync_keys(event_sink: &druid::ExtEventSink, keys: Vec<String>) {
    event_sink.add_idle_callback(move |data: &mut RedisViewerState| {
        data.keys = Vector::from(keys.clone());
        let sender = data.sender.to_owned();
        let keys_senders: Vec<ItemSender> = keys
            .iter()
            .map(|key| {
                let key_sender = ItemSender {
                    sender: sender.clone(),
                    value: key.to_string(),
                };
                key_sender
            })
            .collect();
        data.keys_senders = Vector::from(keys_senders);
        data.is_refreshing = false;
    });
}

#[derive(Clone, Data, Lens)]
struct RedisViewerState {
    sender: Arc<Sender<RedisViewerEvent>>,
    keys: Vector<String>,
    keys_senders: Vector<ItemSender>,
    is_refreshing: bool,
    is_connection_form_showing: bool,
    connection_address: Arc<String>,
    connection_port: Arc<String>,
    connection_db: Arc<String>,
    redis_value: Arc<Option<RedisValue>>,
}

#[derive(Clone, Data, Lens)]
struct ItemSender {
    value: String,
    sender: Arc<Sender<RedisViewerEvent>>,
}

fn build_ui() -> impl Widget<RedisViewerState> {
    let mut root = Flex::column();

    let viewer = build_viewer();

    let connection_form = build_connection_form();

    let either = Either::new(
        |data, _env| data.is_connection_form_showing,
        connection_form,
        viewer,
    );
    root.add_flex_child(either, 1.0);

    root
}

fn build_connection_form() -> impl Widget<RedisViewerState> {
    let mut connection_form = Flex::column();

    connection_form.add_child(Label::new("Address:").fix_height(30.0).expand_width());
    connection_form.add_child(
        TextBox::new()
            .with_placeholder("Address")
            .fix_height(30.0)
            .expand_width()
            .lens(RedisViewerState::connection_address),
    );
    connection_form.add_child(Label::new("Port:").fix_height(30.0).expand_width());
    connection_form.add_child(
        TextBox::new()
            .with_placeholder("Port")
            .fix_height(30.0)
            .expand_width()
            .lens(RedisViewerState::connection_port),
    );
    connection_form.add_child(Label::new("Database:").fix_height(30.0).expand_width());
    connection_form.add_child(
        TextBox::new()
            .with_placeholder("Database")
            .fix_height(30.0)
            .expand_width()
            .lens(RedisViewerState::connection_db),
    );
    connection_form.add_child(
        Button::new("Connect")
            .on_click(|_, data: &mut RedisViewerState, _| {
                data.sender
                    .send(RedisViewerEvent::CreateConnection(
                        data.connection_address.to_string(),
                        data.connection_port.to_string(),
                        data.connection_db.to_string(),
                    ))
                    .expect("failed to send create connection event")
            })
            .fix_height(30.0)
            .expand_width(),
    );
    connection_form
}

fn build_viewer() -> impl Widget<RedisViewerState> {
    let mut viewer = Flex::column();
    let mut top_controls = Flex::row();
    top_controls.add_flex_child(
        Button::new("Refresh")
            .on_click(|_, data: &mut RedisViewerState, _| {
                if !data.is_refreshing {
                    data.is_refreshing = true;
                    data.sender
                        .send(RedisViewerEvent::RefreshKeys)
                        .expect("failed to send refresh keys event");
                }
            })
            .fix_height(30.0)
            .expand_width(),
        1.0,
    );
    viewer.add_child(top_controls);

    let mut bottom_panel = Flex::row();

    let mut keys_list = Flex::column();
    keys_list.add_flex_child(
        Scroll::new(List::new(|| {
            Flex::row()
                .with_flex_child(
                    Button::new(|item: &ItemSender, _env: &_| item.value.clone())
                        .on_click(|_, item: &mut ItemSender, _| {
                            println!("clicked {}", item.value);
                            item.sender
                                .send(RedisViewerEvent::SelectRedisValue(item.value.clone()))
                                .expect("failed to send select redis value event");
                        })
                        .expand_width(),
                    1.0,
                )
                .padding(10.0)
                .background(Color::rgb(0.1, 0.8, 0.1))
                .fix_height(50.0)
                .width(1000.0)
        }))
        .vertical()
        .lens(RedisViewerState::keys_senders),
        1.0,
    );
    bottom_panel.add_flex_child(keys_list.align_left(), 1.0);

    let value_viewer = build_value_viewer();
    bottom_panel.add_flex_child(value_viewer.align_left(), 1.0);

    viewer.add_flex_child(bottom_panel, 1.0);
    viewer.background(Color::rgb(0.1, 0.1, 0.9))
}

fn build_value_viewer() -> impl Widget<RedisViewerState> {
    let mut value_viewer = Flex::column();
    let value_view = Scroll::new(ViewSwitcher::new(
        |data: &RedisViewerState, _env: &_| data.redis_value.clone(),
        |selector, _data, _env| match selector.as_ref() {
            Some(redis_value) => match redis_value {
                RedisValue::String(value) => Box::new(Label::new(value.to_string())),
                RedisValue::List(value_list) => {
                    let mut list_view = Flex::column();
                    for value in value_list {
                        list_view.add_child(Label::new(value.to_string()));
                    }

                    Box::new(list_view)
                }
                RedisValue::Set(value_list) => {
                    let mut list_view = Flex::column();
                    for value in value_list {
                        list_view.add_child(Label::new(value.to_string()));
                    }

                    Box::new(list_view)
                }
                RedisValue::ZSet(value_list) => {
                    let mut list_view = Flex::column();
                    for (v1, v2) in value_list {
                        list_view.add_child(Label::new(v1.to_string()));
                        list_view.add_child(Label::new(v2.to_string()));
                    }

                    Box::new(list_view)
                }
                RedisValue::Hash(hash) => {
                    let mut hash_view = Flex::column();
                    for (k, v) in hash {
                        hash_view.add_child(Label::new(k.to_string()));
                        hash_view.add_child(Label::new(v.to_string()));
                    }

                    Box::new(hash_view)
                }
                RedisValue::Null => {
                    let mut col = Flex::column();
                    col.add_child(Label::new("null"));
                    Box::new(col)
                }
            },
            None => Box::new(Flex::column()),
        },
    ));
    value_viewer.add_flex_child(value_view, 1.0);
    value_viewer
        .expand_width()
        .background(Color::rgb(0.5, 0.0, 0.5))
}
