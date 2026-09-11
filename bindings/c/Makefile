# C ABI acceptance harness.
#
# Builds the cdylib (debug), compiles the harness against the cbindgen
# header with -Wall -Wextra -Werror, links, and runs it.
#
# Usage:
#   make test            # build lib + compile + run (default)
#   make FEATURES=...    # override cargo features (default: system,cloud)
#   make clean

CARGO ?= cargo
CC ?= cc
FEATURES ?= system,cloud

# macOS adds -Wall-ish defaults; keep flags identical everywhere.
CFLAGS = -std=c11 -Wall -Wextra -Werror -I../../include

UNAME_S := $(shell uname -s)
ifeq ($(UNAME_S),Darwin)
  LIB := ../../target/debug/librust_tts_wrapper.dylib
  LOAD := DYLD_LIBRARY_PATH=../../target/debug
else
  LIB := ../../target/debug/librust_tts_wrapper.so
  LOAD := LD_LIBRARY_PATH=../../target/debug
endif

HARNESS := tts_abi_harness

.PHONY: test lib run clean

test: run

lib:
	$(CARGO) build -p rust-tts-wrapper --no-default-features --features $(FEATURES)

$(HARNESS): tts_abi_harness.c lib
	$(CC) $(CFLAGS) tts_abi_harness.c -o $(HARNESS) -L../../target/debug -lrust_tts_wrapper

run: $(HARNESS)
	$(LOAD) ./$(HARNESS)

clean:
	rm -f $(HARNESS)
