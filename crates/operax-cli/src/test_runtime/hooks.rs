pub const OPERAX_HOOK_MARKER: &str = "__greenticOperaxManagerSubmitHook";

pub fn patch_hooks_source(source: &str) -> Result<String, String> {
    if source.contains(OPERAX_HOOK_MARKER) {
        return Ok(source.to_string());
    }
    let needle = "    const result = next(action);\n";
    let replacement = r#"    if (isGreenticOperaxManagerSubmitAction(action)) {
      handleGreenticOperaxManagerSubmit(store, action.payload.activity);
      return;
    }
    if (isGreenticOperaxManagerOpenAction(action)) {
      handleGreenticOperaxManagerOpen(store, action.payload.activity);
      return;
    }

    const result = next(action);
"#;
    if !source.contains(needle) {
        return Err("unable to find WebChat hook middleware insertion point".to_string());
    }
    let mut patched = source.replacen(needle, replacement, 1);
    patched.push_str(OPERAX_MANAGER_HOOK_JS);
    Ok(patched)
}

pub const OPERAX_MANAGER_HOOK_JS: &str = r#"

// OperaX manager Adaptive Cards are rendered as WebChat card assets, but their
// submit and open actions need to call the live manager API.
var __greenticOperaxManagerSubmitHook = true;

function isGreenticOperaxManagerSubmitAction(action) {
  var activity = action && action.payload && action.payload.activity;
  var value = activity && activity.value;
  return action && action.type === 'DIRECT_LINE/POST_ACTIVITY' &&
    value && value.action === 'operax_manager_submit';
}

function isGreenticOperaxManagerOpenAction(action) {
  var activity = action && action.payload && action.payload.activity;
  var value = activity && activity.value;
  return action && action.type === 'DIRECT_LINE/POST_ACTIVITY' &&
    value && (value.action === 'operax_manager_open' || value.manager_target);
}

function greenticOperaxHeaderValue(value, fallback) {
  return value == null || value === '' ? fallback : String(value);
}

function greenticOperaxHeaders(value) {
  var locale = document.documentElement.getAttribute('lang') ||
    document.querySelector('[data-webchat-locale]')?.getAttribute('data-webchat-locale') ||
    'en';
  return {
    'Content-Type': 'application/json',
    'Accept': 'application/json',
    'X-Greentic-Tenant-Id': greenticOperaxHeaderValue(window.__TENANT__, 'demo'),
    'X-Greentic-Caller-Id': greenticOperaxHeaderValue(window.__GUEST_ID__, 'webchat-user'),
    'X-Greentic-Caller-Role': 'operator',
    'X-Greentic-Channel': 'webchat',
    'X-Greentic-Locale': locale,
    'Accept-Language': locale
  };
}

function greenticOperaxCardsBase(value) {
  if (value.manager_cards_base_url && !String(value.manager_cards_base_url).startsWith('/')) {
    window.__GREENTIC_OPERAX_MANAGER_CARDS_BASE_URL__ = value.manager_cards_base_url;
    return value.manager_cards_base_url;
  }
  if (value.manager_submit_url && !String(value.manager_submit_url).startsWith('/')) {
    var derived = String(value.manager_submit_url).replace(/\/submit(?:[?#].*)?$/, '/cards');
    window.__GREENTIC_OPERAX_MANAGER_CARDS_BASE_URL__ = derived;
    return derived;
  }
  return window.__GREENTIC_OPERAX_MANAGER_CARDS_BASE_URL__ || null;
}

function greenticOperaxSubmitUrl(value) {
  if (value.manager_submit_url) return value.manager_submit_url;
  var base = greenticOperaxCardsBase(value);
  return base ? String(base).replace(/\/cards\/?$/, '/submit') : null;
}

function greenticOperaxCardUrl(value) {
  var base = greenticOperaxCardsBase(value);
  var target = value.manager_target || 'dashboard';
  return base ? String(base).replace(/\/+$/, '') + '/' + String(target) : null;
}

function greenticOperaxNormalizeCard(value, sourceValue) {
  var cardsBase = greenticOperaxCardsBase(sourceValue || {});
  if (!cardsBase || value == null) return value;
  if (Array.isArray(value)) {
    value.forEach(function (item) { greenticOperaxNormalizeCard(item, sourceValue); });
    return value;
  }
  if (typeof value !== 'object') return value;
  if (value.type === 'Action.Submit') {
    value.data = value.data || {};
    if (value.data.manager_target || value.data.action === 'operax_manager_open') {
      value.data.manager_cards_base_url = cardsBase;
      value.data.action = value.data.action || 'operax_manager_open';
    }
    if (value.data.action === 'operax_manager_submit') {
      value.data.manager_cards_base_url = cardsBase;
      value.data.manager_submit_url = String(cardsBase).replace(/\/cards\/?$/, '/submit');
    }
  }
  Object.keys(value).forEach(function (key) {
    greenticOperaxNormalizeCard(value[key], sourceValue);
  });
  return value;
}

function greenticOperaxInput(value) {
  if (value.input !== undefined) return value.input;
  if (value.json !== undefined) return value.json;
  if (value.payload !== undefined) return value.payload;
  if (value.input_json !== undefined) {
    try { return JSON.parse(value.input_json); } catch (_) { return value.input_json; }
  }
  var input = {};
  Object.keys(value || {}).forEach(function (key) {
    if (/^(action|cardId|routeToCardId|step|manager_|_)/.test(key)) return;
    if (key === 'operation' || key === 'mode') return;
    input[key] = value[key];
  });
  return input;
}

function greenticOperaxIncomingCardActivity(card) {
  return {
    type: 'message',
    id: 'greentic-operax-manager-' + Date.now(),
    timestamp: new Date().toISOString(),
    from: { id: 'operax-manager', name: 'OperaX Manager', role: 'bot' },
    attachments: [{ contentType: 'application/vnd.microsoft.card.adaptive', content: card }]
  };
}

function greenticOperaxIncomingTextActivity(text) {
  return {
    type: 'message',
    id: 'greentic-operax-manager-error-' + Date.now(),
    timestamp: new Date().toISOString(),
    from: { id: 'operax-manager', name: 'OperaX Manager', role: 'bot' },
    text: text
  };
}

async function handleGreenticOperaxManagerSubmit(store, activity) {
  var value = Object.assign({}, activity && activity.value || {});
  var submitUrl = greenticOperaxSubmitUrl(value);
  if (!submitUrl) return;
  greenticOperaxCardsBase(value);
  var body = Object.assign({}, value, { input: greenticOperaxInput(value) });
  try {
    var submitResponse = await fetch(submitUrl, {
      method: 'POST',
      headers: greenticOperaxHeaders(value),
      body: JSON.stringify(body)
    });
    if (!submitResponse.ok) throw new Error('OperaX manager submit failed with HTTP ' + submitResponse.status);
    var card = greenticOperaxNormalizeCard(await submitResponse.json(), value);
    store.dispatch({
      type: 'DIRECT_LINE/INCOMING_ACTIVITY',
      payload: { activity: greenticOperaxIncomingCardActivity(card) }
    });
  } catch (err) {
    console.error('[operax-manager-submit]', err);
    store.dispatch({
      type: 'DIRECT_LINE/INCOMING_ACTIVITY',
      payload: { activity: greenticOperaxIncomingTextActivity('Unable to submit this OperaX manager form. Please try again.') }
    });
  }
}

async function handleGreenticOperaxManagerOpen(store, activity) {
  var value = Object.assign({}, activity && activity.value || {});
  var cardUrl = greenticOperaxCardUrl(value);
  if (!cardUrl) return;
  try {
    var cardResponse = await fetch(cardUrl, {
      method: 'GET',
      headers: greenticOperaxHeaders(value)
    });
    if (!cardResponse.ok) throw new Error('OperaX manager card load failed with HTTP ' + cardResponse.status);
    var card = greenticOperaxNormalizeCard(await cardResponse.json(), value);
    store.dispatch({
      type: 'DIRECT_LINE/INCOMING_ACTIVITY',
      payload: { activity: greenticOperaxIncomingCardActivity(card) }
    });
  } catch (err) {
    console.error('[operax-manager-open]', err);
    store.dispatch({
      type: 'DIRECT_LINE/INCOMING_ACTIVITY',
      payload: { activity: greenticOperaxIncomingTextActivity('Unable to open this OperaX manager card. Please try again.') }
    });
  }
}
"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn patch_adds_operax_handlers() {
        let source = "function createStoreMiddleware(store) {\n  return next => action => {\n    const result = next(action);\n    return result;\n  };\n}\n";
        let patched = patch_hooks_source(source).unwrap();
        assert!(patched.contains(OPERAX_HOOK_MARKER));
        assert!(patched.contains("operax_manager_submit"));
        assert!(patched.contains("operax_manager_open"));
        assert!(patched.contains("isGreenticOperaxManagerSubmitAction"));
        assert!(patched.contains("handleGreenticOperaxManagerSubmit"));
    }

    #[test]
    fn patch_is_idempotent() {
        let source = "function createStoreMiddleware(store) {\n  return next => action => {\n    const result = next(action);\n    return result;\n  };\n}\n";
        let patched = patch_hooks_source(source).unwrap();
        let double_patched = patch_hooks_source(&patched).unwrap();
        assert_eq!(patched, double_patched);
    }

    #[test]
    fn patch_rejects_missing_insertion_point() {
        let source = "function unrelated() { return 42; }\n";
        let err = patch_hooks_source(source).unwrap_err();
        assert!(err.contains("insertion point"));
    }

    #[test]
    fn patch_returns_source_unchanged_when_already_patched() {
        let source = format!("already has {OPERAX_HOOK_MARKER} marker");
        let result = patch_hooks_source(&source).unwrap();
        assert_eq!(result, source);
    }

    #[test]
    fn hook_js_contains_required_functions() {
        assert!(OPERAX_MANAGER_HOOK_JS.contains("isGreenticOperaxManagerSubmitAction"));
        assert!(OPERAX_MANAGER_HOOK_JS.contains("isGreenticOperaxManagerOpenAction"));
        assert!(OPERAX_MANAGER_HOOK_JS.contains("handleGreenticOperaxManagerSubmit"));
        assert!(OPERAX_MANAGER_HOOK_JS.contains("handleGreenticOperaxManagerOpen"));
        assert!(OPERAX_MANAGER_HOOK_JS.contains("greenticOperaxHeaders"));
        assert!(OPERAX_MANAGER_HOOK_JS.contains("greenticOperaxInput"));
        assert!(OPERAX_MANAGER_HOOK_JS.contains("greenticOperaxNormalizeCard"));
    }
}
