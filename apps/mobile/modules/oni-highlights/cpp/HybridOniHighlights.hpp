#pragma once

#include "HybridOniHighlightsSpec.hpp"

namespace margelo::nitro::onihighlights {

class HybridOniHighlights : public HybridOniHighlightsSpec {
public:
  HybridOniHighlights() : HybridObject(TAG) {}

  std::optional<std::shared_ptr<ArrayBuffer>> tokenize(SourceText source,
                                                       double language) override;
};

} // namespace margelo::nitro::onihighlights
