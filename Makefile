# Makefile — build testing.c under several different compiler settings,
# then run mini_decompiler against each resulting binary so you can
# compare how optimization/stripping/linking affects decompiled output.
#
# Usage:
#   make            # build all variants + run decompiler on all of them
#   make build       # just build the binaries
#   make decompile   # just run the decompiler (build implied)
#   make show-O2     # build+decompile+print just the -O2 variant
#   make clean

UNAME_S := $(shell uname -s)

ifeq ($(UNAME_S),Darwin)
CC           := clang
ARCH_FLAGS   := -arch x86_64
COMMON_FLAGS := -fno-stack-protector $(ARCH_FLAGS)
STRIP        := strip -x
ALL_VARIANTS := O0 O1 O2 O3 stripped pie obj
else
CC           := gcc
ARCH_FLAGS   :=
COMMON_FLAGS := -fno-stack-protector -no-pie
STRIP        := strip --strip-all
ALL_VARIANTS := O0 O1 O2 O3 static stripped static_stripped pie obj
endif
SRC         := testing.c
BUILD_DIR   := bin
DECOMP_DIR  := decompiled

# Detect whether SRC actually defines main(). The `obj` variant is
# compile-only (gcc -c) and never needs one, but O0/O1/O2/O3/static/
# stripped/pie all link a full executable and will fail at the ld step
# without it. If SRC is a header-less "library" file (free functions
# only, meant to be read via the obj variant), restrict the build to
# `obj` instead of failing 7 ways on missing `main`.
# (LP holds a literal "(" via plain assignment so the regex below doesn't
# put an unbalanced paren directly inside the $(shell ...) call, which
# would confuse make's own paren matching.)
LP := \(
# Strip comments via the preprocessor first (-E -P), otherwise a comment
# that merely *mentions* "main(" -- like this file's own header comment --
# would false-positive the check.
HAS_MAIN := $(shell $(CC) -E -P $(SRC) 2>/dev/null | grep -qE '(^|[^A-Za-z0-9_])main[[:space:]]*$(LP)' && echo 1 || echo 0)

# Path to the mini_decompiler cargo project. In this layout the Makefile
# lives at the crate root (next to Cargo.toml/src), so it's just ".".
DECOMPILER_DIR := .
DECOMPILER_BIN := $(DECOMPILER_DIR)/target/release/mini_decompiler
MAX_FUNCS      := 30

# Common flags: no stack protector / no PIE so addresses+disasm stay simple
# and easy to read; each variant then adds/overrides on top of this.

# variant-name -> extra gcc flags
ifeq ($(HAS_MAIN),1)
VARIANTS      := $(ALL_VARIANTS)
else
VARIANTS      := obj
$(info note: no main() found in $(SRC) -- only building the 'obj' (compile-only) variant; O0/O1/O2/O3/static/stripped/pie all need a linked executable and would fail at the link step.)
endif
FLAGS.O0      := -O0
FLAGS.O1      := -O1
FLAGS.O2      := -O2
FLAGS.O3      := -O3
FLAGS.static  := -O0 -static
FLAGS.stripped:= -O0
FLAGS.static_stripped := -O0 -static
FLAGS.pie     := -O0 -pie -fpie

BINARIES := $(addprefix $(BUILD_DIR)/testing_,$(VARIANTS))
REPORTS  := $(addprefix $(DECOMP_DIR)/testing_,$(addsuffix .txt,$(VARIANTS)))

.PHONY: all build decompile clean $(addprefix show-,$(VARIANTS)) decompiler

all: decompile
$(info building for x86-64 with $(CC)$(if $(ARCH_FLAGS), $(ARCH_FLAGS),) on $(UNAME_S))

#build the decompiler itself
decompiler: $(DECOMPILER_BIN)

$(DECOMPILER_BIN):
	@echo "==> building mini_decompiler (release)"
	cargo build --release --manifest-path $(DECOMPILER_DIR)/Cargo.toml

#build all binary variants
build: $(BINARIES)

$(BUILD_DIR):
	mkdir -p $(BUILD_DIR)

# pie variant conflicts with -no-pie in COMMON_FLAGS, so it overrides fully
$(BUILD_DIR)/testing_pie: $(SRC) | $(BUILD_DIR)
	@echo "==> [pie] $(CC) $(FLAGS.pie) -o $@ $(SRC)"
	$(CC) -fno-stack-protector $(ARCH_FLAGS) $(FLAGS.pie) -o $@ $(SRC)

# object file — compile only, never linked, so no `main` is required.
# Useful for library-style testing.c files that are just free functions.
$(BUILD_DIR)/testing_obj: $(SRC) | $(BUILD_DIR)
	@echo "==> [obj] $(CC) -Wall -fno-stack-protector -c -o $@ $(SRC)  (no main required)"
	$(CC) -Wall -fno-stack-protector $(ARCH_FLAGS) -c -o $@ $(SRC)

# static variant also can't take -no-pie the same way on some toolchains,
# gcc handles -static -no-pie together fine though, so keep it simple.
$(BUILD_DIR)/testing_static: $(SRC) | $(BUILD_DIR)
	@echo "==> [static] $(CC) $(COMMON_FLAGS) $(FLAGS.static) -o $@ $(SRC)"
	$(CC) $(COMMON_FLAGS) $(FLAGS.static) -o $@ $(SRC)

$(BUILD_DIR)/testing_stripped: $(SRC) | $(BUILD_DIR)
	@echo "==> [stripped] $(CC) $(COMMON_FLAGS) $(FLAGS.stripped) -o $@ $(SRC), then strip"
	$(CC) $(COMMON_FLAGS) $(FLAGS.stripped) -o $@ $(SRC)
	$(STRIP) $@

$(BUILD_DIR)/testing_static_stripped: $(SRC) | $(BUILD_DIR)
	@echo "==> [static_stripped] $(CC) $(COMMON_FLAGS) $(FLAGS.static_stripped) -o $@ $(SRC), then strip"
	$(CC) $(COMMON_FLAGS) $(FLAGS.static_stripped) -o $@ $(SRC)
	$(STRIP) $@

$(BUILD_DIR)/testing_O0: $(SRC) | $(BUILD_DIR)
	@echo "==> [O0] $(CC) $(COMMON_FLAGS) $(FLAGS.O0) -o $@ $(SRC)"
	$(CC) $(COMMON_FLAGS) $(FLAGS.O0) -o $@ $(SRC)

$(BUILD_DIR)/testing_O1: $(SRC) | $(BUILD_DIR)
	@echo "==> [O1] $(CC) $(COMMON_FLAGS) $(FLAGS.O1) -o $@ $(SRC)"
	$(CC) $(COMMON_FLAGS) $(FLAGS.O1) -o $@ $(SRC)

$(BUILD_DIR)/testing_O2: $(SRC) | $(BUILD_DIR)
	@echo "==> [O2] $(CC) $(COMMON_FLAGS) $(FLAGS.O2) -o $@ $(SRC)"
	$(CC) $(COMMON_FLAGS) $(FLAGS.O2) -o $@ $(SRC)

$(BUILD_DIR)/testing_O3: $(SRC) | $(BUILD_DIR)
	@echo "==> [O3] $(CC) $(COMMON_FLAGS) $(FLAGS.O3) -o $@ $(SRC)"
	$(CC) $(COMMON_FLAGS) $(FLAGS.O3) -o $@ $(SRC)

#run the decompiler against every variant
decompile: $(REPORTS)

$(DECOMP_DIR):
	mkdir -p $(DECOMP_DIR)

$(DECOMP_DIR)/testing_%.txt: $(BUILD_DIR)/testing_% $(DECOMPILER_BIN) | $(DECOMP_DIR)
	@echo "==> decompiling $<"
	@echo "############################################" >  $@
	@echo "# variant: $*"                                 >> $@
	@echo "# binary : $<"                                  >> $@
	@file $<                                                >> $@
	@echo "############################################" >> $@
	$(DECOMPILER_BIN) $< $(MAX_FUNCS)                       >> $@
	@echo "    -> $@"

#build+decompile+print one variant on demand
show-%: $(DECOMP_DIR)/testing_%.txt
	@cat $<

clean:
	rm -rf $(BUILD_DIR) $(DECOMP_DIR)