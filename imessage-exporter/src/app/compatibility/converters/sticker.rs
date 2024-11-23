/*!
 Defines routines for converting sticker image files.
*/

use std::{
    fs::{create_dir_all, read_dir, remove_dir_all},
    path::{Path, PathBuf},
};

use imessage_database::tables::attachment::MediaType;

use crate::app::compatibility::{
    converters::{
        common::{copy_raw, ensure_paths, run_command},
        image::convert_heic,
    },
    models::{ImageConverter, ImageType, VideoConverter},
};

/// Copy a sticker, converting if possible
///
/// - Sticker `HEIC` files convert to `PNG`
/// - Sticker `HEICS` files convert to `GIF`
/// - Fallback to the original format
pub(crate) fn sticker_copy_convert(
    from: &Path,
    to: &mut PathBuf,
    image_converter: &ImageConverter,
    video_converter: &Option<VideoConverter>,
    mime_type: MediaType,
) -> Option<MediaType<'static>> {
    // Determine the output type of the sticker
    let output_type: Option<ImageType> = match mime_type {
        // Normal stickers get converted to png
        MediaType::Image("heic") | MediaType::Image("HEIC") => Some(ImageType::Png),
        MediaType::Image("heics")
        | MediaType::Image("HEICS")
        | MediaType::Image("heic-sequence") => Some(ImageType::Gif),
        _ => None,
    };

    if let Some(output_type) = output_type {
        to.set_extension(output_type.to_str());
        // If the attachment is an animated sticker, attempt to convert it to a gif
        // Fall back to the normal converter if this fails
        if matches!(output_type, ImageType::Gif) {
            if let Some(video_converter) = video_converter {
                if convert_heics(from, to, video_converter).is_some() {
                    return Some(MediaType::Image(output_type.to_str()));
                }
            }
        }

        // Standard `HEIC` converter
        if convert_heic(from, to, image_converter, &output_type).is_none() {
            eprintln!("Unable to convert {from:?}");
        } else {
            return Some(MediaType::Image(output_type.to_str()));
        }
    }

    copy_raw(from, to);
    None
}

fn convert_heics(from: &Path, to: &Path, video_converter: &VideoConverter) -> Option<()> {
    let (from_path, to_path) = ensure_paths(from, to)?;

    // Frames per second in the original sticker, generated by Apple
    let fps = 10;

    // Directory to store intermediate renders
    let tmp_path = PathBuf::from("/tmp/imessage");
    // Ensure the temp directory tree exists
    if !tmp_path.exists() {
        if let Err(why) = create_dir_all(&tmp_path) {
            eprintln!("Unable to create {tmp_path:?}: {why}");
            return None;
        }
    }
    let tmp = tmp_path.to_str()?;

    match video_converter {
        VideoConverter::Ffmpeg => {
            // HEICS format contains 4 video streams
            // The first one is the first still
            // Stream #0:0[0x1]: Video: hevc (Main) (hvc1 / 0x31637668), yuv420p(tv, smpte170m/unknown/unknown), 524x600, 1 fps, 1 tbr, 1 tbn (default)
            // The second one is the alpha mask for the first still
            // Stream #0:1[0x2]: Video: hevc (Rext) (hvc1 / 0x31637668), gray(pc), 524x600, 1 fps, 1 tbr, 1 tbn

            // The third stream is the video data
            // Stream #0:2[0x1](und): Video: hevc (Main) (hvc1 / 0x31637668), yuv420p(tv, smpte170m/unknown/unknown), 524x600, 1370 kb/s, 22.98 fps, 30 tbr, 600 tbn (default)
            run_command(
                "ffmpeg",
                vec![
                    "-i",
                    from_path,
                    "-map",
                    "0:2",
                    "-y",
                    &format!("{tmp}/frame_%04d.png"),
                ],
            )?;

            // The fourth stream is the alpha mask
            // Stream #0:3[0x2](und): Video: hevc (Rext) (hvc1 / 0x31637668), gray(pc), 524x600, 426 kb/s, 22.98 fps, 30 tbr, 600 tbn (default)
            run_command(
                "ffmpeg",
                vec![
                    "-i",
                    from_path,
                    "-map",
                    "0:3",
                    "-y",
                    &format!("{tmp}/alpha_%04d.png"),
                ],
            )?;

            // This step applies the transparency mask to the images
            let files = read_dir(tmp).ok()?;
            let num_frames = &files.into_iter().count() / 2;
            (0..num_frames).try_for_each(|item| {
                run_command(
                    "ffmpeg",
                    vec![
                        "-i",
                        &format!("{tmp}/frame_{:04}.png", item),
                        "-i",
                        &format!("{tmp}/alpha_{:04}.png", item),
                        "-filter_complex",
                        "[1:v]format=gray,geq=lum='p(X,Y)':a='p(X,Y)'[mask];[0:v][mask]alphamerge",
                        &format!("{tmp}/merged_{:04}.png", item),
                    ],
                )
            })?;

            // Once we have the transparent frames,
            // we use the first frame to generate a transparency palette
            run_command(
                "ffmpeg",
                vec![
                    "-i",
                    &format!("{tmp}/merged_0001.png"),
                    "-vf",
                    "palettegen=reserve_transparent=1",
                    &format!("{tmp}/palette.png"),
                ],
            )?;

            // Create the gif from the parts we parsed above
            run_command(
                "ffmpeg",
                vec![
                    "-i",
                    &format!("{tmp}/merged_%04d.png"),
                    "-i",
                    &format!("{tmp}/palette.png"),
                    "-lavfi",
                    &format!("fps={fps},paletteuse=alpha_threshold=128"),
                    "-gifflags",
                    "-offsetting",
                    to_path,
                ],
            )?;

            // Remove all of the generated files
            remove_dir_all(tmp).ok()?;

            Some(())
        }
    }
}
