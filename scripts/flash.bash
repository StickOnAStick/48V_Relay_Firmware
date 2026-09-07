#!/bin/bash
espflash flash \
  --port /dev/ttyACM2 --chip esp32 \
  --before no-reset --no-stub --baud 115200 --monitor \
  target/xtensa-esp32-none-elf/release/relay-board-five-chn