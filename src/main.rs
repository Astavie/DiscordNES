use discord::channel::{Channel, ChannelResource};
use discord::gateway::{Gateway, GatewayEvent};
use discord::interaction::{AnyInteraction, ComponentInteractionResource, CreateUpdate, Webhook};
use discord::message::{
    ActionRow, ActionRowComponent, Attachment, Button, ButtonStyle, CreateAttachment, CreateMessage,
};
use discord::request::{Bot, File, IndexedOr, Result};
use discord::resource::Snowflake;
use dotenv::dotenv;
use fastnes::ppu::DrawOptions;
use fastnes::{input::Controllers, nes::NES, ppu::FastPPU};
use futures_util::stream::StreamExt;
use image::codecs::gif::GifEncoder;
use image::{ColorType, ImageOutputFormat};
use std::env;
use std::io::Cursor;
use std::io::Write;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::Arc;

type Frame = [fastnes::ppu::Color; 61440];

fn encode_frame<W: Write>(gif: &mut GifEncoder<W>, nes: &mut NES) {
    nes.next_frame();
    let frame = nes.draw_frame(DrawOptions::All);
    gif.encode(
        unsafe {
            ::core::slice::from_raw_parts(
                (&frame as *const Frame) as *const u8,
                ::core::mem::size_of::<Frame>(),
            )
        },
        256,
        240,
        ColorType::Rgba8,
    )
    .unwrap();
}

fn as_png(frame: &Frame, name: String) -> File {
    let mut buffer = Cursor::new(Vec::new());
    image::write_buffer_with_format(
        &mut buffer,
        unsafe {
            ::core::slice::from_raw_parts(
                (frame as *const Frame) as *const u8,
                ::core::mem::size_of::<Frame>(),
            )
        },
        256,
        240,
        ColorType::Rgba8,
        ImageOutputFormat::Png,
    )
    .unwrap();
    let data = buffer.into_inner();

    File {
        name,
        typ: "image/png".into(),
        data: data.into(),
    }
}

fn components(input: u8) -> Vec<ActionRow> {
    let button = |custom_id: &str, label: Option<&str>, bit: Option<u8>| {
        ActionRowComponent::Button(Button::Action {
            style: if let Some(bit) = bit {
                if input & 1 << bit == 0 {
                    ButtonStyle::Primary
                } else {
                    ButtonStyle::Success
                }
            } else {
                ButtonStyle::Secondary
            },
            custom_id: custom_id.into(),
            disabled: label.is_none(),
            label: Some(label.unwrap_or("_").into()),
        })
    };
    vec![
        ActionRow::new(vec![
            button("00", None, None),
            button("up", Some("⬆"), Some(4)),
            button("02", None, None),
            button("03", None, None),
            button("04", None, None),
        ]),
        ActionRow::new(vec![
            button("left", Some("⬅"), Some(6)),
            button("11", None, None),
            button("right", Some("➡"), Some(7)),
            button("13", None, None),
            button("a", Some("🅰️"), Some(0)),
        ]),
        ActionRow::new(vec![
            button("20", None, None),
            button("down", Some("⬇"), Some(5)),
            button("22", None, None),
            button("b", Some("🅱️"), Some(1)),
            button("24", None, None),
        ]),
        ActionRow::new(vec![
            button("next", Some("Next"), None),
            button("reset", Some("Reset"), None),
        ]),
    ]
}

async fn display(
    client: &Bot,
    nes: &mut NES,
    input: u8,
    channel: Snowflake<Channel>,
) -> Result<Snowflake<Attachment>> {
    let frame = nes.draw_frame(DrawOptions::All);
    let img = as_png(&frame, "frame.png".into());

    let msg = channel
        .send_message(
            &client,
            CreateMessage::default()
                .components(components(input))
                .attachments(vec![CreateAttachment::new(img)].into()),
        )
        .await?;

    Ok(msg.attachments[0].id)
}

fn can_control_mario(nes: &NES) -> bool {
    nes.read_internal(0x000e) == 8
}

async fn run() -> Result<()> {
    // create emulator
    let input = Arc::new(AtomicU8::new(0));
    let controllers = Controllers::standard(&input);
    let mut nes = NES::read_ines("rom/smb.nes", controllers, FastPPU::new());

    // run until 1-1
    for _ in 0..60 {
        nes.next_frame();
    }

    input.store(1 << 3, Ordering::Relaxed);
    nes.next_frame();
    input.store(0, Ordering::Relaxed);

    for _ in 0..60 {
        nes.next_frame();
    }
    while !can_control_mario(&nes) {
        nes.next_frame();
    }

    // load dotenv
    dotenv().unwrap();
    let token = env::var("TOKEN").expect("Bot token TOKEN must be set");
    let channel: Snowflake<Channel> = env::var("CHANNEL")
        .expect("CHANNEL must be set")
        .try_into()
        .expect("CHANNEL is not a valid channel id");

    // connect
    let client = Bot::new(token);

    // channel to test in
    let mut attachment = display(&client, &mut nes, 0, channel).await?;

    // gateway
    let mut gateway = Gateway::connect(&client).await?;
    while let Some(event) = gateway.next().await {
        match event {
            GatewayEvent::InteractionCreate(AnyInteraction::Component(i)) => {
                // flip input
                let mut byte = input.load(Ordering::Relaxed);
                byte ^= 1
                    << match i.data.custom_id.as_str() {
                        "a" => 0,
                        "b" => 1,
                        "up" => 4,
                        "down" => 5,
                        "left" => 6,
                        "right" => 7,
                        "next" => {
                            let mut bytes = Vec::new();
                            let mut gif = GifEncoder::new_with_speed(&mut bytes, 30);

                            // run emu for 10 frames
                            for _ in 0..5 {
                                // the GIF encoder cannot succeed 30fps while the game runs at 60
                                // so we only show half the frames
                                nes.next_frame();
                                encode_frame(&mut gif, &mut nes);
                            }
                            while !can_control_mario(&nes) {
                                nes.next_frame();
                                encode_frame(&mut gif, &mut nes);
                            }
                            drop(gif);

                            // display
                            let img = File {
                                name: "frames.gif".into(),
                                typ: "image/gif".into(),
                                data: bytes.into(),
                            };

                            let msg = i
                                .update(
                                    &Webhook,
                                    CreateUpdate::default()
                                        .components(components(byte))
                                        .attachments(IndexedOr(
                                            vec![CreateAttachment::new(img)],
                                            vec![],
                                        )),
                                )
                                .await?
                                .get(&Webhook)
                                .await?;

                            attachment = msg.attachments[0].id;
                            continue;
                        }
                        "reset" => {
                            nes.reset();
                            input.store(0, Ordering::Relaxed);

                            // run until 1-1
                            for _ in 0..60 {
                                nes.next_frame();
                            }

                            input.store(1 << 3, Ordering::Relaxed);
                            nes.next_frame();
                            input.store(0, Ordering::Relaxed);

                            for _ in 0..60 {
                                nes.next_frame();
                            }
                            while !can_control_mario(&nes) {
                                nes.next_frame();
                            }

                            // display
                            let frame = nes.draw_frame(DrawOptions::All);
                            let img = as_png(&frame, "frame.png".into());

                            let msg = i
                                .update(
                                    &Webhook,
                                    CreateUpdate::default()
                                        .components(components(byte))
                                        .attachments(IndexedOr(
                                            vec![CreateAttachment::new(img)],
                                            vec![],
                                        )),
                                )
                                .await?
                                .get(&Webhook)
                                .await?;

                            attachment = msg.attachments[0].id;
                            continue;
                        }
                        _ => continue,
                    };
                input.store(byte, Ordering::Relaxed);

                // display
                i.update(
                    &Webhook,
                    CreateUpdate::default()
                        .components(components(byte))
                        .attachments(IndexedOr(vec![], vec![attachment.into()])),
                )
                .await?;
            }
            _ => {}
        }
    }
    gateway.close().await;
    Ok(())
}

#[tokio::main]
async fn main() {
    run().await.unwrap()
}
