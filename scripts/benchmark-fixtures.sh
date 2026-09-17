#!/bin/sh
# Generate the optional P0 media set with an explicitly selected FFmpeg.
set -eu

ffmpeg_dir=${1:-${SLICER_FFMPEG_DIR:-}}
output_root=${2:-build/validation/p0-fixtures}
if [ -z "$ffmpeg_dir" ]; then
    echo "benchmark-fixtures: set SLICER_FFMPEG_DIR or pass an FFmpeg bin directory" >&2
    exit 2
fi
ffmpeg=$ffmpeg_dir/ffmpeg
ffprobe=$ffmpeg_dir/ffprobe
if [ ! -x "$ffmpeg" ] || [ ! -x "$ffprobe" ]; then
    echo "benchmark-fixtures: expected executable ffmpeg and ffprobe in $ffmpeg_dir" >&2
    exit 2
fi

media=$output_root/media
frames=$output_root/numbered-frames
mkdir -p "$media" "$frames" "$output_root"

"$ffmpeg" -hide_banner -loglevel error -f lavfi -i "color=c=black:s=1920x1080:r=30" \
    -vf "drawtext=text='P0 frame %{n}':x=40:y=40:fontsize=48:fontcolor=white" \
    -frames:v 90 -c:v mpeg4 -q:v 4 -an -y "$media/moving-1080p30-short-gop.mp4"
"$ffmpeg" -hide_banner -loglevel error -f lavfi -i "testsrc2=size=1920x1080:rate=30:duration=3" \
    -g 90 -c:v mpeg4 -q:v 4 -an -y "$media/moving-1080p30-long-gop.mp4"
"$ffmpeg" -hide_banner -loglevel error -f lavfi -i "testsrc2=size=1920x1080:rate=30:duration=3" \
    -f lavfi -i "sine=frequency=440:sample_rate=44100:duration=3" -shortest \
    -map 0:v:0 -map 1:a:0 -c:v mpeg4 -q:v 4 -c:a aac -y "$media/moving-1080p30-audio-44k.mp4"
"$ffmpeg" -hide_banner -loglevel error -f lavfi -i "testsrc2=size=1920x1080:rate=60:duration=3" \
    -f lavfi -i "sine=frequency=880:sample_rate=48000:duration=3" -shortest \
    -map 0:v:0 -map 1:a:0 -c:v mpeg4 -q:v 4 -c:a aac -y "$media/moving-1080p60-audio-48k.mp4"
"$ffmpeg" -hide_banner -loglevel error -f lavfi -i "testsrc2=size=3840x2160:rate=30:duration=2" \
    -c:v mpeg4 -q:v 5 -an -y "$media/moving-4k30-video-only.mkv"
"$ffmpeg" -hide_banner -loglevel error -f lavfi -i "testsrc2=size=640x360:rate=24:duration=3" \
    -vf "setpts=(N+floor(N/3))*1/(24*TB)" -fps_mode vfr -c:v mpeg4 -q:v 5 -an \
    -y "$media/variable-frame-rate.mp4"
"$ffmpeg" -hide_banner -loglevel error -f lavfi -i "testsrc2=size=640x360:rate=30:duration=2" \
    -vf "setsar=4/3" -metadata:s:v:0 rotate=90 -c:v mpeg4 -q:v 5 -an \
    -y "$media/rotation-and-sar.mp4"
mkdir -p "$media/unicode café 日本"
"$ffmpeg" -hide_banner -loglevel error -f lavfi -i "testsrc2=size=320x180:rate=24:duration=2" \
    -c:v mpeg4 -q:v 5 -an -metadata title='Unicode café 日本語' \
    -y "$media/unicode café 日本/moving-text-日本.mp4"
"$ffmpeg" -hide_banner -loglevel error -f lavfi -i "color=c=red@0.5:s=64x64,format=rgba" \
    -frames:v 1 -f image2 -y "$media/alpha-overlay.png"

i=0
while [ "$i" -lt 24 ]; do
    # The numbered files are deliberately simple and deterministic; the
    # compositor benchmark does not depend on them for its pixel oracle.
    r=$((16 + (i * 7) % 200))
    g=$((24 + (i * 11) % 180))
    b=$((40 + (i * 13) % 160))
    {
        echo P3
        echo 16 9
        echo 255
        awk -v r="$r" -v g="$g" -v b="$b" 'BEGIN { for (i=0; i<144; i++) printf "%d %d %d\n", r, g, b }'
    } > "$frames/frame-$(printf '%03d' "$i").ppm"
    i=$((i + 1))
done

"$ffmpeg" -hide_banner -loglevel error -f lavfi -i "sine=frequency=1:sample_rate=44100:duration=3" \
    -c:a pcm_s16le -y "$media/impulse-44k.wav"
"$ffmpeg" -hide_banner -loglevel error -f lavfi -i "sine=frequency=1:sample_rate=48000:duration=3" \
    -c:a pcm_s16le -y "$media/impulse-48k.wav"

"$ffprobe" -hide_banner -loglevel error -show_format -show_streams -print_format json \
    -- "$media/moving-1080p30-audio-44k.mp4" > "$output_root/ffprobe-sample.json"
printf '%s\n' '{"schema":"slicer.p0-fixture-generation.v1","status":"generated","media_root":"media"}' \
    > "$output_root/fixture-generation.json"
echo "benchmark-fixtures: generated $output_root"

