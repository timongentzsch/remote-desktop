#!/bin/sh
# Builds the test images, one for each kind of desktop and one each for building, for the far end of a link
# and for a browser. They are named rd-test-<kind>.
cd "$(dirname "$0")/images"
for image in ${@:-build sway oldsway gnome kde x11 net chrome}; do
  echo "== rd-test-$image"
  docker build -q -t rd-test-$image -f $image.Dockerfile . || exit 1
done
