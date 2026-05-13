if (!Array.prototype.forEach) {
  // Simplified iterator for browsers without forEach support
  Array.prototype.forEach = function(cb) {
    if (typeof this.length != 'number') return;
    if (typeof callback != 'function') return;

    for (var i = 0; i < this.length; i++) cb(this[i]);
  }
}

function copyToClipboard(text) {
  const element = document.createElement("textarea");
  element.style.display = "none;"
  element.value = text;

  document.body.appendChild(element);
  element.focus();
  element.setSelectionRange(0, element.value.length);

  document.execCommand("copy");
  document.body.removeChild(element);
}

function guid() {
  var cryptoObj = null;
  if (typeof globalThis !== "undefined" && globalThis.crypto) {
    cryptoObj = globalThis.crypto;
  }
  else if (typeof window !== "undefined" && window.crypto) {
    cryptoObj = window.crypto;
  }
  else if (typeof window !== "undefined" && window.msCrypto) {
    cryptoObj = window.msCrypto;
  }

  if (!cryptoObj || !cryptoObj.getRandomValues) {
    throw new Error("secure random source is unavailable");
  }

  var bytes = new Uint8Array(16);
  cryptoObj.getRandomValues(bytes);
  bytes[6] = (bytes[6] & 0x0f) | 0x40;
  bytes[8] = (bytes[8] & 0x3f) | 0x80;

  var hex = [];
  for (var i = 0; i < bytes.length; i++) {
    hex.push((bytes[i] + 0x100).toString(16).substring(1));
  }
  return [
    hex[0], hex[1], hex[2], hex[3], "-",
    hex[4], hex[5], "-",
    hex[6], hex[7], "-",
    hex[8], hex[9], "-",
    hex[10], hex[11], hex[12], hex[13], hex[14], hex[15]
  ].join("");
}
