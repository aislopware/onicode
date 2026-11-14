// Terminal state shared by the Android (JNI) and iOS (Swift) renderers, on top of
// libghostty-vt. Plain C so both bridges call it directly.
#ifndef ONI_TERMINAL_CORE_H
#define ONI_TERMINAL_CORE_H

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef struct OniTerminal OniTerminal;

// Heap bytes the caller owns and releases with oni_terminal_bytes_free.
typedef struct {
  uint8_t* data;
  size_t len;
} OniTerminalBytes;

// Colors are 0xAARRGGBB; alpha is ignored. An empty palette keeps the current one.
typedef struct {
  uint32_t foreground;
  uint32_t background;
  uint32_t cursor;
  const uint32_t* palette;
  size_t palette_len;
} OniTerminalTheme;

OniTerminal* oni_terminal_create(int32_t cols, int32_t rows, int32_t cell_width,
                                 int32_t cell_height, const OniTerminalTheme* theme);
void oni_terminal_destroy(OniTerminal* terminal);

// Functions returning bytes return the PTY replies the call produced.
OniTerminalBytes oni_terminal_feed(OniTerminal* terminal, const uint8_t* data, size_t len);
OniTerminalBytes oni_terminal_resize(OniTerminal* terminal, int32_t cols, int32_t rows,
                                     int32_t cell_width, int32_t cell_height);
OniTerminalBytes oni_terminal_set_theme(OniTerminal* terminal, const OniTerminalTheme* theme);

// Positive rows scroll toward the newest output.
void oni_terminal_scroll(OniTerminal* terminal, int32_t rows);
// A wheel or swipe over viewport cell (col, row): mouse reports while the app tracks the
// mouse, arrow keys on the alternate screen, otherwise a viewport scroll with no bytes.
OniTerminalBytes oni_terminal_scroll_wheel(OniTerminal* terminal, int32_t rows, int32_t col,
                                           int32_t row);

bool oni_terminal_select_word_at(OniTerminal* terminal, int32_t col, int32_t row);
void oni_terminal_extend_selection(OniTerminal* terminal, int32_t anchor_col, int32_t anchor_row,
                                   int32_t col, int32_t row);
bool oni_terminal_select_all(OniTerminal* terminal);
void oni_terminal_clear_selection(OniTerminal* terminal);
// Plain UTF-8 with soft wraps joined and trailing whitespace trimmed. False when there is
// no selection.
bool oni_terminal_selection_text(OniTerminal* terminal, OniTerminalBytes* out);
// The visible rows, formatted like oni_terminal_selection_text.
bool oni_terminal_viewport_text(OniTerminal* terminal, OniTerminalBytes* out);

// Little-endian frame of the viewport, empty on failure:
//   u32 magic "ONVT", u16 version 1, u16 cols, u16 rows, u16 cursor x, u16 cursor y
//   (0xFFFF when outside the viewport), u8 cursor visible, u8 cursor style, u8 blinking,
//   u8 0, u32 foreground, u32 background, u32 cursor color,
//   then cols*rows cells: u32 fg, u32 bg, u16 flags, u16 text length, UTF-8 text.
// Cell colors already apply inverse, faint and the minimum contrast.
OniTerminalBytes oni_terminal_snapshot(OniTerminal* terminal);

void oni_terminal_bytes_free(OniTerminalBytes bytes);

// Cell flags in the snapshot.
enum {
  ONI_TERMINAL_CELL_BOLD = 1 << 0,
  ONI_TERMINAL_CELL_ITALIC = 1 << 1,
  ONI_TERMINAL_CELL_FAINT = 1 << 2,
  ONI_TERMINAL_CELL_INVERSE = 1 << 3,
  ONI_TERMINAL_CELL_INVISIBLE = 1 << 4,
  ONI_TERMINAL_CELL_STRIKETHROUGH = 1 << 5,
  ONI_TERMINAL_CELL_OVERLINE = 1 << 6,
  ONI_TERMINAL_CELL_UNDERLINE = 1 << 7,
  ONI_TERMINAL_CELL_SELECTED = 1 << 8,
};

#ifdef __cplusplus
}
#endif

#endif  // ONI_TERMINAL_CORE_H
