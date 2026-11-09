#pragma once

#include <react/renderer/components/OniSourceTextSpec/EventEmitters.h>
#include <react/renderer/components/OniSourceTextSpec/Props.h>
#include <react/renderer/components/OniSourceTextSpec/States.h>
#include <react/renderer/components/view/ConcreteViewShadowNode.h>

namespace facebook::react {
extern const char OniSourceTextRunComponentName[];

using OniSourceTextRunShadowNode =
    ConcreteViewShadowNode<OniSourceTextRunComponentName, OniSourceTextRunProps,
                           OniSourceTextRunEventEmitter, OniSourceTextRunState>;
} // namespace facebook::react
