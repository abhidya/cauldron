#!/bin/bash
# Records the demo: starts the app on a virtual display, captures the screen, runs the director.
# usage: record.sh <out.mp4>
set -u
HERE="$(cd "$(dirname "$0")" && pwd)"
OUT="${1:-/tmp/cauldron-demo.mp4}"
"$HERE/start.sh" || exit 1
sleep 8
rm -f /tmp/cauldron-raw.mp4
ffmpeg -loglevel error -y -f x11grab -draw_mouse 0 -framerate 20 -video_size 1280x800 -i :99 -c:v libx264 -preset ultrafast -crf 20 -pix_fmt yuv420p /tmp/cauldron-raw.mp4 &
FF=$!
sleep 1.5
python3 "$HERE/director.py" --marks /tmp/cauldron-marks.json; RC=$?
sleep 1
kill -INT $FF; wait $FF 2>/dev/null
[ $RC -eq 0 ] || { echo "director failed"; exit 1; }
# Shorten the wait for the network clone (between the marks "1 clone running" and "2 ingest") to about four seconds.
read A B <<<"$(python3 -I -c "
import json
m={x['scene']:x['at'] for x in json.load(open('/tmp/cauldron-marks.json'))}
print(m['1 clone running']+3.0, m['2 ingest']+1.0)")"
ffmpeg -loglevel error -y -i /tmp/cauldron-raw.mp4 -filter_complex "[0:v]trim=0:$A,setpts=PTS-STARTPTS[a];[0:v]trim=$A:$B,setpts=(PTS-STARTPTS)/8[b];[0:v]trim=start=$B,setpts=PTS-STARTPTS[c];[a][b][c]concat=n=3:v=1[v]" -map "[v]" -r 20 -c:v libx264 -preset medium -crf 22 -pix_fmt yuv420p -movflags +faststart "$OUT"
echo "wrote $OUT"
