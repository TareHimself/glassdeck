#!/bin/sh
# Runs inside Keyglass's firmware build image, with Keyglass at /kg and this folder at /vectors.
set -eu
program=$1
cd /kg/keyboard/firmware
srcs=$(make -s --eval='print-srcs: ; @echo $(CORE_SRCS)' print-srcs)
lfs=../../third_party/littlefs
cc -std=c11 -O2 -DHOST_TEST -DFW_PROJECT_VERSION='"vectors"' -Isrc -I$lfs -Wno-unused-function \
  -o "/tmp/$program" "/vectors/$program.c" $srcs $lfs/lfs.c $lfs/lfs_util.c
"/tmp/$program"
