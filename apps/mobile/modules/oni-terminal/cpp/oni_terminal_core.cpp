#include "oni_terminal_core.h"

#include <algorithm>
#include <cmath>
#include <cstdint>
#include <cstdlib>
#include <cstring>
#include <mutex>
#include <unordered_map>
#include <utility>
#include <vector>

#include <ghostty/vt.h>

struct OniTerminal {
  GhosttyTerminal terminal = nullptr;
  GhosttyRenderState render_state = nullptr;
  GhosttyRenderStateRowIterator row_iterator = nullptr;
  GhosttyRenderStateRowCells row_cells = nullptr;
  GhosttyMouseEncoder mouse_encoder = nullptr;
  GhosttyMouseEvent mouse_event = nullptr;
  uint32_t cell_width = 1;
  uint32_t cell_height = 1;
  std::vector<uint8_t> responses;
  std::unordered_map<uint64_t, GhosttyColorRgb> contrast_cache;
  GhosttyColorScheme color_scheme = GHOSTTY_COLOR_SCHEME_DARK;
  std::mutex mutex;
};

namespace {

constexpr uint32_t kSnapshotMagic = 0x54564E4F;  // "ONVT" in little endian.
constexpr uint16_t kSnapshotVersion = 1;
// Match Ghostty's default scrollback-limit.
constexpr size_t kMaxScrollbackBytes = 10'000'000;
// Below this relative luminance white text contrasts more than black.
constexpr double kDarkBackgroundLuminance = 0.179;
// Matches the desktop terminal (apps/web/src/terminal/ghostty/contrast.ts): WCAG AA, halved for faint.
constexpr double kMinimumContrast = 4.5;
constexpr size_t kContrastCacheLimit = 4096;

class ByteWriter {
 public:
  explicit ByteWriter(size_t capacity) { bytes_.reserve(capacity); }

  void U8(uint8_t value) { bytes_.push_back(value); }

  void U16(uint16_t value) {
    U8(static_cast<uint8_t>(value));
    U8(static_cast<uint8_t>(value >> 8));
  }

  void U32(uint32_t value) {
    U16(static_cast<uint16_t>(value));
    U16(static_cast<uint16_t>(value >> 16));
  }

  void Bytes(const std::vector<uint8_t>& value) {
    bytes_.insert(bytes_.end(), value.begin(), value.end());
  }

  std::vector<uint8_t>& Data() { return bytes_; }

 private:
  std::vector<uint8_t> bytes_;
};

OniTerminalBytes CopyBytes(const uint8_t* data, size_t len) {
  if (data == nullptr || len == 0) return OniTerminalBytes{nullptr, 0};
  auto* copy = static_cast<uint8_t*>(std::malloc(len));
  if (copy == nullptr) return OniTerminalBytes{nullptr, 0};
  std::memcpy(copy, data, len);
  return OniTerminalBytes{copy, len};
}

OniTerminalBytes CopyBytes(const std::vector<uint8_t>& bytes) {
  return CopyBytes(bytes.data(), bytes.size());
}

GhosttyColorRgb RgbFromArgb(uint32_t value) {
  return {
      .r = static_cast<uint8_t>(value >> 16),
      .g = static_cast<uint8_t>(value >> 8),
      .b = static_cast<uint8_t>(value),
  };
}

uint32_t ArgbFromRgb(GhosttyColorRgb color) {
  return 0xFF000000U | (static_cast<uint32_t>(color.r) << 16U) |
         (static_cast<uint32_t>(color.g) << 8U) | color.b;
}

GhosttyColorRgb Blend(GhosttyColorRgb foreground, GhosttyColorRgb background,
                      uint8_t foreground_weight) {
  const auto blend = [foreground_weight](uint8_t front, uint8_t back) {
    const uint16_t back_weight = 255 - foreground_weight;
    return static_cast<uint8_t>((front * foreground_weight + back * back_weight) / 255);
  };
  return {
      .r = blend(foreground.r, background.r),
      .g = blend(foreground.g, background.g),
      .b = blend(foreground.b, background.b),
  };
}

double ChannelLuminance(uint8_t channel) {
  const double value = channel / 255.0;
  return value <= 0.03928 ? value / 12.92 : std::pow((value + 0.055) / 1.055, 2.4);
}

double Luminance(GhosttyColorRgb color) {
  return 0.2126 * ChannelLuminance(color.r) + 0.7152 * ChannelLuminance(color.g) +
         0.0722 * ChannelLuminance(color.b);
}

double ContrastRatio(double left, double right) {
  return (std::max(left, right) + 0.05) / (std::min(left, right) + 0.05);
}

// Smallest mix toward black or white that reaches `ratio`; mixing keeps the hue.
GhosttyColorRgb MixForContrast(GhosttyColorRgb color, double background_luminance, double ratio,
                               bool toward_white, double* reached) {
  const double target = toward_white ? 255.0 : 0.0;
  const auto mix = [&](double amount) {
    // Rounding toward the target never lowers contrast below the searched mix.
    const auto channel = [&](uint8_t value) {
      const double mixed = value + (target - value) * amount;
      return static_cast<uint8_t>(toward_white ? std::ceil(mixed) : std::floor(mixed));
    };
    return GhosttyColorRgb{.r = channel(color.r), .g = channel(color.g), .b = channel(color.b)};
  };
  const auto ratio_of = [&](GhosttyColorRgb candidate) {
    return ContrastRatio(Luminance(candidate), background_luminance);
  };
  const auto extreme = mix(1.0);
  if (ratio_of(extreme) < ratio) {
    *reached = ratio_of(extreme);
    return extreme;
  }
  double low = 0.0;
  double high = 1.0;
  for (int step = 0; step < 10; ++step) {
    const double middle = (low + high) / 2.0;
    if (ratio_of(mix(middle)) >= ratio) {
      high = middle;
    } else {
      low = middle;
    }
  }
  const auto adjusted = mix(high);
  *reached = ratio_of(adjusted);
  return adjusted;
}

GhosttyColorRgb EnsureContrast(GhosttyColorRgb foreground, GhosttyColorRgb background,
                               double ratio) {
  const double background_luminance = Luminance(background);
  const double foreground_luminance = Luminance(foreground);
  if (ContrastRatio(foreground_luminance, background_luminance) >= ratio) return foreground;
  const bool lighten_first = foreground_luminance >= background_luminance;
  double preferred_ratio = 0.0;
  const auto preferred = MixForContrast(foreground, background_luminance, ratio, lighten_first,
                                        &preferred_ratio);
  if (preferred_ratio >= ratio) return preferred;
  double fallback_ratio = 0.0;
  const auto fallback = MixForContrast(foreground, background_luminance, ratio, !lighten_first,
                                       &fallback_ratio);
  return fallback_ratio > preferred_ratio ? fallback : preferred;
}

// Powerline separators, block elements and legacy-computing glyphs must match the
// neighbouring cell's background.
bool IsContrastExempt(uint32_t codepoint) {
  return (codepoint >= 0x2580 && codepoint <= 0x259F) ||
         (codepoint >= 0xE0B0 && codepoint <= 0xE0D7) ||
         (codepoint >= 0x1FB00 && codepoint <= 0x1FBFF) ||
         (codepoint >= 0x1CC00 && codepoint <= 0x1CEBF);
}

// A truecolor fg/bg pair was designed together by the app, e.g. powerline segments.
bool HasTruecolorPair(const GhosttyStyle& style) {
  return style.fg_color.tag == GHOSTTY_STYLE_COLOR_RGB &&
         style.bg_color.tag == GHOSTTY_STYLE_COLOR_RGB;
}

bool SameColor(GhosttyColorRgb left, GhosttyColorRgb right) {
  return left.r == right.r && left.g == right.g && left.b == right.b;
}

GhosttyColorRgb ContrastedForeground(OniTerminal* session, GhosttyColorRgb foreground,
                                     GhosttyColorRgb background, bool faint) {
  const uint64_t key = (static_cast<uint64_t>(ArgbFromRgb(foreground) & 0xFFFFFFU) << 25U) |
                       (static_cast<uint64_t>(ArgbFromRgb(background) & 0xFFFFFFU) << 1U) |
                       (faint ? 1U : 0U);
  const auto cached = session->contrast_cache.find(key);
  if (cached != session->contrast_cache.end()) return cached->second;
  const auto adjusted =
      EnsureContrast(foreground, background, faint ? kMinimumContrast / 2 : kMinimumContrast);
  if (session->contrast_cache.size() >= kContrastCacheLimit) session->contrast_cache.clear();
  session->contrast_cache.emplace(key, adjusted);
  return adjusted;
}

void AppendUtf8(std::vector<uint8_t>* output, uint32_t codepoint) {
  if (codepoint <= 0x7F) {
    output->push_back(static_cast<uint8_t>(codepoint));
  } else if (codepoint <= 0x7FF) {
    output->push_back(static_cast<uint8_t>(0xC0 | (codepoint >> 6)));
    output->push_back(static_cast<uint8_t>(0x80 | (codepoint & 0x3F)));
  } else if (codepoint <= 0xFFFF && !(codepoint >= 0xD800 && codepoint <= 0xDFFF)) {
    output->push_back(static_cast<uint8_t>(0xE0 | (codepoint >> 12)));
    output->push_back(static_cast<uint8_t>(0x80 | ((codepoint >> 6) & 0x3F)));
    output->push_back(static_cast<uint8_t>(0x80 | (codepoint & 0x3F)));
  } else if (codepoint <= 0x10FFFF) {
    output->push_back(static_cast<uint8_t>(0xF0 | (codepoint >> 18)));
    output->push_back(static_cast<uint8_t>(0x80 | ((codepoint >> 12) & 0x3F)));
    output->push_back(static_cast<uint8_t>(0x80 | ((codepoint >> 6) & 0x3F)));
    output->push_back(static_cast<uint8_t>(0x80 | (codepoint & 0x3F)));
  }
}

void OnWritePty(GhosttyTerminal, void* userdata, const uint8_t* data, size_t len) {
  auto* session = static_cast<OniTerminal*>(userdata);
  if (session == nullptr || data == nullptr || len == 0) return;
  session->responses.insert(session->responses.end(), data, data + len);
}

bool OnColorScheme(GhosttyTerminal, void* userdata, GhosttyColorScheme* out_scheme) {
  auto* session = static_cast<OniTerminal*>(userdata);
  if (session == nullptr) return false;
  *out_scheme = session->color_scheme;
  return true;
}

bool ModeEnabled(OniTerminal* session, GhosttyMode mode) {
  GhosttyTerminalModeConfig config{.mode = mode, .value = false};
  return ghostty_terminal_get(session->terminal, GHOSTTY_TERMINAL_DATA_MODE, &config) ==
             GHOSTTY_SUCCESS &&
         config.value;
}

// Mode 2031 asks for an unsolicited CSI ? 997 report whenever the scheme changes.
void ReportColorSchemeChange(OniTerminal* session) {
  if (!ModeEnabled(session, GHOSTTY_MODE_REPORT_COLOR_SCHEME)) return;
  uint8_t report[16];
  size_t written = 0;
  if (ghostty_color_scheme_report_encode(session->color_scheme, report, sizeof(report),
                                         &written) == GHOSTTY_SUCCESS) {
    session->responses.insert(session->responses.end(), report, report + written);
  }
}

void ApplyTheme(OniTerminal* session, const OniTerminalTheme& theme) {
  auto foreground_rgb = RgbFromArgb(theme.foreground);
  auto background_rgb = RgbFromArgb(theme.background);
  auto cursor_rgb = RgbFromArgb(theme.cursor);
  ghostty_terminal_set(session->terminal, GHOSTTY_TERMINAL_OPT_COLOR_FOREGROUND,
                       &foreground_rgb);
  ghostty_terminal_set(session->terminal, GHOSTTY_TERMINAL_OPT_COLOR_BACKGROUND,
                       &background_rgb);
  ghostty_terminal_set(session->terminal, GHOSTTY_TERMINAL_OPT_COLOR_CURSOR, &cursor_rgb);

  const auto scheme = Luminance(background_rgb) < kDarkBackgroundLuminance
                          ? GHOSTTY_COLOR_SCHEME_DARK
                          : GHOSTTY_COLOR_SCHEME_LIGHT;
  if (scheme != session->color_scheme) {
    session->color_scheme = scheme;
    ReportColorSchemeChange(session);
  }

  if (theme.palette == nullptr || theme.palette_len == 0) return;

  GhosttyColorRgb palette[256];
  if (ghostty_terminal_get(session->terminal, GHOSTTY_TERMINAL_DATA_COLOR_PALETTE_DEFAULT,
                           palette) != GHOSTTY_SUCCESS) {
    return;
  }
  const auto copied_length = std::min<size_t>(theme.palette_len, 256);
  for (size_t index = 0; index < copied_length; ++index) {
    palette[index] = RgbFromArgb(theme.palette[index]);
  }
  ghostty_terminal_set(session->terminal, GHOSTTY_TERMINAL_OPT_COLOR_PALETTE, palette);
}

OniTerminalBytes DrainResponses(OniTerminal* session) {
  auto bytes = CopyBytes(session->responses);
  session->responses.clear();
  return bytes;
}

uint16_t ClampGrid(int32_t value) { return static_cast<uint16_t>(std::clamp(value, 1, 65535)); }

uint32_t ClampCell(int32_t value) { return static_cast<uint32_t>(std::max(value, 1)); }

void Resize(OniTerminal* session, int32_t cols, int32_t rows, int32_t cell_width,
            int32_t cell_height) {
  session->cell_width = ClampCell(cell_width);
  session->cell_height = ClampCell(cell_height);
  ghostty_terminal_resize(session->terminal, ClampGrid(cols), ClampGrid(rows),
                          session->cell_width, session->cell_height);
}

bool ViewportGridRef(OniTerminal* session, int32_t x, int32_t y, GhosttyGridRef* out) {
  *out = GhosttyGridRef{};
  GhosttyPoint point{};
  point.tag = GHOSTTY_POINT_TAG_VIEWPORT;
  point.value.coordinate.x = static_cast<uint16_t>(std::max<int32_t>(x, 0));
  point.value.coordinate.y = static_cast<uint32_t>(std::max<int32_t>(y, 0));
  return ghostty_terminal_grid_ref(session->terminal, point, out) == GHOSTTY_SUCCESS;
}

bool SetSelection(OniTerminal* session, const GhosttySelection* selection) {
  return ghostty_terminal_set(session->terminal, GHOSTTY_TERMINAL_OPT_SELECTION, selection) ==
         GHOSTTY_SUCCESS;
}

bool FormatSelection(OniTerminal* session, const GhosttySelection& selection,
                     OniTerminalBytes* out) {
  GhosttyFormatter formatter = nullptr;
  if (ghostty_formatter_terminal_new(nullptr, &formatter, session->terminal) !=
      GHOSTTY_SUCCESS) {
    return false;
  }
  const auto emit = GHOSTTY_FORMATTER_FORMAT_PLAIN;
  const bool enabled = true;
  uint8_t* bytes = nullptr;
  size_t len = 0;
  const bool formatted =
      ghostty_formatter_set(formatter, GHOSTTY_FORMATTER_OPT_EMIT, &emit) == GHOSTTY_SUCCESS &&
      ghostty_formatter_set(formatter, GHOSTTY_FORMATTER_OPT_UNWRAP, &enabled) ==
          GHOSTTY_SUCCESS &&
      ghostty_formatter_set(formatter, GHOSTTY_FORMATTER_OPT_TRIM, &enabled) ==
          GHOSTTY_SUCCESS &&
      ghostty_formatter_set(formatter, GHOSTTY_FORMATTER_OPT_SELECTION, &selection) ==
          GHOSTTY_SUCCESS &&
      ghostty_formatter_format_alloc(formatter, nullptr, &bytes, &len) == GHOSTTY_SUCCESS;
  ghostty_formatter_free(formatter);
  if (!formatted || bytes == nullptr) return false;
  *out = CopyBytes(bytes, len);
  ghostty_free(nullptr, bytes, len);
  return true;
}

bool HasStyleFlag(const GhosttyStyle& style, GhosttyStyleFlags flag) {
  return (style.flags & flag) != 0;
}

uint16_t StyleFlags(const GhosttyStyle& style, bool selected) {
  uint16_t flags = 0;
  if (HasStyleFlag(style, GHOSTTY_STYLE_FLAG_BOLD)) flags |= ONI_TERMINAL_CELL_BOLD;
  if (HasStyleFlag(style, GHOSTTY_STYLE_FLAG_ITALIC)) flags |= ONI_TERMINAL_CELL_ITALIC;
  if (HasStyleFlag(style, GHOSTTY_STYLE_FLAG_FAINT)) flags |= ONI_TERMINAL_CELL_FAINT;
  if (HasStyleFlag(style, GHOSTTY_STYLE_FLAG_INVERSE)) flags |= ONI_TERMINAL_CELL_INVERSE;
  if (HasStyleFlag(style, GHOSTTY_STYLE_FLAG_INVISIBLE)) flags |= ONI_TERMINAL_CELL_INVISIBLE;
  if (HasStyleFlag(style, GHOSTTY_STYLE_FLAG_STRIKETHROUGH)) {
    flags |= ONI_TERMINAL_CELL_STRIKETHROUGH;
  }
  if (HasStyleFlag(style, GHOSTTY_STYLE_FLAG_OVERLINE)) flags |= ONI_TERMINAL_CELL_OVERLINE;
  if (GHOSTTY_STYLE_UNDERLINE(style) != 0) flags |= ONI_TERMINAL_CELL_UNDERLINE;
  if (selected) flags |= ONI_TERMINAL_CELL_SELECTED;
  return flags;
}

bool EncodeWheel(OniTerminal* session, int32_t rows, int32_t col, int32_t row) {
  if (session->mouse_encoder == nullptr &&
      ghostty_mouse_encoder_new(nullptr, &session->mouse_encoder) != GHOSTTY_SUCCESS) {
    return false;
  }
  if (session->mouse_event == nullptr &&
      ghostty_mouse_event_new(nullptr, &session->mouse_event) != GHOSTTY_SUCCESS) {
    return false;
  }
  uint16_t cols = 1;
  uint16_t grid_rows = 1;
  ghostty_terminal_get(session->terminal, GHOSTTY_TERMINAL_DATA_COLS, &cols);
  ghostty_terminal_get(session->terminal, GHOSTTY_TERMINAL_DATA_ROWS, &grid_rows);
  ghostty_mouse_encoder_set_from_terminal(session->mouse_encoder, session->terminal);
  auto size = GHOSTTY_INIT_SIZED(GhosttyMouseEncoderSize);
  size.screen_width = cols * session->cell_width;
  size.screen_height = grid_rows * session->cell_height;
  size.cell_width = session->cell_width;
  size.cell_height = session->cell_height;
  ghostty_mouse_encoder_set(session->mouse_encoder, GHOSTTY_MOUSE_ENCODER_OPT_SIZE, &size);

  const auto clamped_col = std::clamp<int32_t>(col, 0, cols - 1);
  const auto clamped_row = std::clamp<int32_t>(row, 0, grid_rows - 1);
  ghostty_mouse_event_set_action(session->mouse_event, GHOSTTY_MOUSE_ACTION_PRESS);
  ghostty_mouse_event_set_button(session->mouse_event,
                                 rows < 0 ? GHOSTTY_MOUSE_BUTTON_FOUR : GHOSTTY_MOUSE_BUTTON_FIVE);
  ghostty_mouse_event_set_mods(session->mouse_event, 0);
  ghostty_mouse_event_set_position(
      session->mouse_event,
      GhosttyMousePosition{
          .x = (static_cast<float>(clamped_col) + 0.5f) * static_cast<float>(session->cell_width),
          .y = (static_cast<float>(clamped_row) + 0.5f) *
               static_cast<float>(session->cell_height)});
  uint8_t buffer[128];
  for (int32_t index = 0, count = std::abs(rows); index < count; ++index) {
    size_t written = 0;
    if (ghostty_mouse_encoder_encode(session->mouse_encoder, session->mouse_event, buffer,
                                     sizeof(buffer), &written) != GHOSTTY_SUCCESS) {
      break;
    }
    session->responses.insert(session->responses.end(), buffer, buffer + written);
  }
  return true;
}

}  // namespace

extern "C" {

OniTerminal* oni_terminal_create(int32_t cols, int32_t rows, int32_t cell_width,
                                 int32_t cell_height, const OniTerminalTheme* theme) {
  auto* session = new OniTerminal();
  if (ghostty_terminal_new(nullptr, &session->terminal, ClampGrid(cols), ClampGrid(rows)) !=
          GHOSTTY_SUCCESS ||
      ghostty_render_state_new(nullptr, &session->render_state) != GHOSTTY_SUCCESS ||
      ghostty_render_state_row_iterator_new(nullptr, &session->row_iterator) != GHOSTTY_SUCCESS ||
      ghostty_render_state_row_cells_new(nullptr, &session->row_cells) != GHOSTTY_SUCCESS) {
    oni_terminal_destroy(session);
    return nullptr;
  }

  ghostty_terminal_set(session->terminal, GHOSTTY_TERMINAL_OPT_SCROLLBACK_MAX_BYTES,
                       &kMaxScrollbackBytes);
  ghostty_terminal_set(session->terminal, GHOSTTY_TERMINAL_OPT_USERDATA, session);
  ghostty_terminal_set(session->terminal, GHOSTTY_TERMINAL_OPT_WRITE_PTY,
                       reinterpret_cast<const void*>(OnWritePty));
  ghostty_terminal_set(session->terminal, GHOSTTY_TERMINAL_OPT_COLOR_SCHEME,
                       reinterpret_cast<const void*>(OnColorScheme));
  if (theme != nullptr) ApplyTheme(session, *theme);
  Resize(session, cols, rows, cell_width, cell_height);
  return session;
}

void oni_terminal_destroy(OniTerminal* session) {
  if (session == nullptr) return;
  ghostty_mouse_event_free(session->mouse_event);
  ghostty_mouse_encoder_free(session->mouse_encoder);
  ghostty_render_state_row_cells_free(session->row_cells);
  ghostty_render_state_row_iterator_free(session->row_iterator);
  ghostty_render_state_free(session->render_state);
  ghostty_terminal_free(session->terminal);
  delete session;
}

OniTerminalBytes oni_terminal_feed(OniTerminal* session, const uint8_t* data, size_t len) {
  if (session == nullptr) return OniTerminalBytes{nullptr, 0};
  std::lock_guard<std::mutex> lock(session->mutex);
  if (data != nullptr && len > 0) ghostty_terminal_vt_write(session->terminal, data, len);
  return DrainResponses(session);
}

OniTerminalBytes oni_terminal_resize(OniTerminal* session, int32_t cols, int32_t rows,
                                     int32_t cell_width, int32_t cell_height) {
  if (session == nullptr) return OniTerminalBytes{nullptr, 0};
  std::lock_guard<std::mutex> lock(session->mutex);
  Resize(session, cols, rows, cell_width, cell_height);
  return DrainResponses(session);
}

OniTerminalBytes oni_terminal_set_theme(OniTerminal* session, const OniTerminalTheme* theme) {
  if (session == nullptr || theme == nullptr) return OniTerminalBytes{nullptr, 0};
  std::lock_guard<std::mutex> lock(session->mutex);
  ApplyTheme(session, *theme);
  return DrainResponses(session);
}

void oni_terminal_scroll(OniTerminal* session, int32_t rows) {
  if (session == nullptr || rows == 0) return;
  std::lock_guard<std::mutex> lock(session->mutex);
  GhosttyTerminalScrollViewport scroll = {
      .tag = GHOSTTY_TERMINAL_SCROLL_VIEWPORT_DELTA,
      .value = {.delta = rows},
  };
  ghostty_terminal_scroll_viewport(session->terminal, scroll);
}

OniTerminalBytes oni_terminal_scroll_wheel(OniTerminal* session, int32_t rows, int32_t col,
                                           int32_t row) {
  if (session == nullptr || rows == 0) return OniTerminalBytes{nullptr, 0};
  std::lock_guard<std::mutex> lock(session->mutex);
  bool tracking = false;
  ghostty_terminal_get(session->terminal, GHOSTTY_TERMINAL_DATA_MOUSE_TRACKING, &tracking);
  if (tracking && EncodeWheel(session, rows, col, row)) return DrainResponses(session);

  auto screen = GHOSTTY_TERMINAL_SCREEN_PRIMARY;
  ghostty_terminal_get(session->terminal, GHOSTTY_TERMINAL_DATA_ACTIVE_SCREEN, &screen);
  if (screen == GHOSTTY_TERMINAL_SCREEN_ALTERNATE) {
    // Alternate screens have no scrollback; send arrow keys like xterm.
    const bool application = ModeEnabled(session, GHOSTTY_MODE_CURSOR_KEYS);
    const uint8_t arrow[3] = {0x1B, static_cast<uint8_t>(application ? 'O' : '['),
                              static_cast<uint8_t>(rows < 0 ? 'A' : 'B')};
    for (int32_t index = 0, count = std::abs(rows); index < count; ++index) {
      session->responses.insert(session->responses.end(), arrow, arrow + sizeof(arrow));
    }
    return DrainResponses(session);
  }

  GhosttyTerminalScrollViewport scroll = {
      .tag = GHOSTTY_TERMINAL_SCROLL_VIEWPORT_DELTA,
      .value = {.delta = rows},
  };
  ghostty_terminal_scroll_viewport(session->terminal, scroll);
  return OniTerminalBytes{nullptr, 0};
}

bool oni_terminal_select_word_at(OniTerminal* session, int32_t col, int32_t row) {
  if (session == nullptr) return false;
  std::lock_guard<std::mutex> lock(session->mutex);
  GhosttyTerminalSelectWordOptions options{};
  options.size = sizeof(options);
  if (!ViewportGridRef(session, col, row, &options.ref)) return false;
  GhosttySelection selection{};
  if (ghostty_terminal_select_word(session->terminal, &options, &selection) != GHOSTTY_SUCCESS) {
    return false;
  }
  return SetSelection(session, &selection);
}

void oni_terminal_extend_selection(OniTerminal* session, int32_t anchor_col, int32_t anchor_row,
                                   int32_t col, int32_t row) {
  if (session == nullptr) return;
  std::lock_guard<std::mutex> lock(session->mutex);
  GhosttySelection selection{};
  if (!ViewportGridRef(session, anchor_col, anchor_row, &selection.start)) return;
  if (!ViewportGridRef(session, col, row, &selection.end)) return;
  SetSelection(session, &selection);
}

bool oni_terminal_select_all(OniTerminal* session) {
  if (session == nullptr) return false;
  std::lock_guard<std::mutex> lock(session->mutex);
  GhosttySelection selection{};
  if (ghostty_terminal_select_all(session->terminal, &selection) != GHOSTTY_SUCCESS) {
    return false;
  }
  return SetSelection(session, &selection);
}

void oni_terminal_clear_selection(OniTerminal* session) {
  if (session == nullptr) return;
  std::lock_guard<std::mutex> lock(session->mutex);
  SetSelection(session, nullptr);
}

bool oni_terminal_selection_text(OniTerminal* session, OniTerminalBytes* out) {
  *out = OniTerminalBytes{nullptr, 0};
  if (session == nullptr) return false;
  std::lock_guard<std::mutex> lock(session->mutex);
  GhosttySelection selection{};
  if (ghostty_terminal_get(session->terminal, GHOSTTY_TERMINAL_DATA_SELECTION, &selection) !=
      GHOSTTY_SUCCESS) {
    return false;
  }
  return FormatSelection(session, selection, out);
}

bool oni_terminal_viewport_text(OniTerminal* session, OniTerminalBytes* out) {
  *out = OniTerminalBytes{nullptr, 0};
  if (session == nullptr) return false;
  std::lock_guard<std::mutex> lock(session->mutex);
  uint16_t cols = 0;
  uint16_t rows = 0;
  ghostty_terminal_get(session->terminal, GHOSTTY_TERMINAL_DATA_COLS, &cols);
  ghostty_terminal_get(session->terminal, GHOSTTY_TERMINAL_DATA_ROWS, &rows);
  if (cols == 0 || rows == 0) return false;
  GhosttySelection selection{};
  if (!ViewportGridRef(session, 0, 0, &selection.start) ||
      !ViewportGridRef(session, cols - 1, rows - 1, &selection.end)) {
    return false;
  }
  return FormatSelection(session, selection, out);
}

OniTerminalBytes oni_terminal_snapshot(OniTerminal* session) {
  if (session == nullptr) return OniTerminalBytes{nullptr, 0};
  std::lock_guard<std::mutex> lock(session->mutex);

  if (ghostty_render_state_update(session->render_state, session->terminal) != GHOSTTY_SUCCESS) {
    return OniTerminalBytes{nullptr, 0};
  }

  uint16_t cols = 0;
  uint16_t rows = 0;
  bool cursor_visible = false;
  bool cursor_in_viewport = false;
  bool cursor_blinking = false;
  uint16_t cursor_x = 0xFFFF;
  uint16_t cursor_y = 0xFFFF;
  GhosttyCursorStyle cursor_style = GHOSTTY_CURSOR_STYLE_BLOCK;
  ghostty_render_state_get(session->render_state, GHOSTTY_RENDER_STATE_DATA_COLS, &cols);
  ghostty_render_state_get(session->render_state, GHOSTTY_RENDER_STATE_DATA_ROWS, &rows);
  ghostty_render_state_get(session->render_state, GHOSTTY_RENDER_STATE_DATA_CURSOR_VISIBLE,
                           &cursor_visible);
  ghostty_render_state_get(session->render_state,
                           GHOSTTY_RENDER_STATE_DATA_CURSOR_VIEWPORT_HAS_VALUE,
                           &cursor_in_viewport);
  ghostty_render_state_get(session->render_state, GHOSTTY_RENDER_STATE_DATA_CURSOR_BLINKING,
                           &cursor_blinking);
  ghostty_render_state_get(session->render_state,
                           GHOSTTY_RENDER_STATE_DATA_CURSOR_VISUAL_STYLE, &cursor_style);
  if (cursor_in_viewport) {
    ghostty_render_state_get(session->render_state,
                             GHOSTTY_RENDER_STATE_DATA_CURSOR_VIEWPORT_X, &cursor_x);
    ghostty_render_state_get(session->render_state,
                             GHOSTTY_RENDER_STATE_DATA_CURSOR_VIEWPORT_Y, &cursor_y);
  }

  auto colors = GHOSTTY_INIT_SIZED(GhosttyRenderStateColors);
  if (ghostty_render_state_get(session->render_state, GHOSTTY_RENDER_STATE_DATA_COLORS,
                               &colors) != GHOSTTY_SUCCESS) {
    return OniTerminalBytes{nullptr, 0};
  }
  const auto cursor_color = colors.cursor_has_value ? colors.cursor : colors.foreground;

  ByteWriter writer(32 + static_cast<size_t>(cols) * rows * 14);
  writer.U32(kSnapshotMagic);
  writer.U16(kSnapshotVersion);
  writer.U16(cols);
  writer.U16(rows);
  writer.U16(cursor_x);
  writer.U16(cursor_y);
  writer.U8(cursor_visible && cursor_in_viewport ? 1 : 0);
  writer.U8(static_cast<uint8_t>(cursor_style));
  writer.U8(cursor_blinking ? 1 : 0);
  writer.U8(0);
  writer.U32(ArgbFromRgb(colors.foreground));
  writer.U32(ArgbFromRgb(colors.background));
  writer.U32(ArgbFromRgb(cursor_color));

  if (ghostty_render_state_get(session->render_state, GHOSTTY_RENDER_STATE_DATA_ROW_ITERATOR,
                               &session->row_iterator) != GHOSTTY_SUCCESS) {
    return OniTerminalBytes{nullptr, 0};
  }

  uint16_t written_rows = 0;
  std::vector<uint32_t> codepoints;
  std::vector<uint8_t> utf8;
  while (written_rows < rows && ghostty_render_state_row_iterator_next(session->row_iterator)) {
    if (ghostty_render_state_row_get(session->row_iterator, GHOSTTY_RENDER_STATE_ROW_DATA_CELLS,
                                     &session->row_cells) != GHOSTTY_SUCCESS) {
      break;
    }

    uint16_t written_cols = 0;
    while (written_cols < cols && ghostty_render_state_row_cells_next(session->row_cells)) {
      GhosttyStyle style{};
      bool selected = false;
      GhosttyColorRgb foreground = colors.foreground;
      GhosttyColorRgb background = colors.background;
      ghostty_render_state_row_cells_get(session->row_cells,
                                         GHOSTTY_RENDER_STATE_ROW_CELLS_DATA_STYLE, &style);
      ghostty_render_state_row_cells_get(session->row_cells,
                                         GHOSTTY_RENDER_STATE_ROW_CELLS_DATA_SELECTED, &selected);
      ghostty_render_state_row_cells_get(session->row_cells,
                                         GHOSTTY_RENDER_STATE_ROW_CELLS_DATA_FG_COLOR,
                                         &foreground);
      ghostty_render_state_row_cells_get(session->row_cells,
                                         GHOSTTY_RENDER_STATE_ROW_CELLS_DATA_BG_COLOR,
                                         &background);
      const bool faint = HasStyleFlag(style, GHOSTTY_STYLE_FLAG_FAINT);
      if (HasStyleFlag(style, GHOSTTY_STYLE_FLAG_INVERSE)) std::swap(foreground, background);
      if (faint) foreground = Blend(foreground, background, 155);

      uint32_t grapheme_count = 0;
      ghostty_render_state_row_cells_get(
          session->row_cells, GHOSTTY_RENDER_STATE_ROW_CELLS_DATA_GRAPHEMES_LEN, &grapheme_count);
      utf8.clear();
      if (grapheme_count > 0) {
        codepoints.assign(grapheme_count, 0);
        GhosttyCodepointBuffer graphemes{
            .ptr = codepoints.data(), .cap = codepoints.size(), .len = 0};
        if (ghostty_render_state_row_cells_get(session->row_cells,
                                               GHOSTTY_RENDER_STATE_ROW_CELLS_DATA_GRAPHEMES_BUF,
                                               &graphemes) == GHOSTTY_SUCCESS) {
          for (const auto codepoint : codepoints) AppendUtf8(&utf8, codepoint);
          // Prompts and TUIs often hardcode colours for one background; keep them legible.
          if (!IsContrastExempt(codepoints[0]) && !HasTruecolorPair(style) &&
              !SameColor(foreground, background) &&
              !(SameColor(foreground, colors.foreground) &&
                SameColor(background, colors.background))) {
            foreground = ContrastedForeground(session, foreground, background, faint);
          }
        }
      }

      const auto text_length = static_cast<uint16_t>(std::min<size_t>(utf8.size(), 65535));
      writer.U32(ArgbFromRgb(foreground));
      writer.U32(ArgbFromRgb(background));
      writer.U16(StyleFlags(style, selected));
      writer.U16(text_length);
      if (text_length != utf8.size()) utf8.resize(text_length);
      writer.Bytes(utf8);
      ++written_cols;
    }

    while (written_cols++ < cols) {
      writer.U32(ArgbFromRgb(colors.foreground));
      writer.U32(ArgbFromRgb(colors.background));
      writer.U16(0);
      writer.U16(0);
    }
    ++written_rows;
  }

  while (written_rows++ < rows) {
    for (uint16_t column = 0; column < cols; ++column) {
      writer.U32(ArgbFromRgb(colors.foreground));
      writer.U32(ArgbFromRgb(colors.background));
      writer.U16(0);
      writer.U16(0);
    }
  }

  return CopyBytes(writer.Data());
}

void oni_terminal_bytes_free(OniTerminalBytes bytes) { std::free(bytes.data); }

}  // extern "C"
