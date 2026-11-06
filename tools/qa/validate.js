// SPDX-FileCopyrightText: 2026 Jamie Jahn
// SPDX-License-Identifier: GPL-3.0-or-later
// Khronos glTF validator over files: npm install gltf-validator && node tools/qa/validate.js out/*.glb
const v = require('gltf-validator'); const fs = require('fs'); const path = require('path');
(async () => {
  for (const f of process.argv.slice(2)) {
    const r = await v.validateBytes(new Uint8Array(fs.readFileSync(f)), {maxIssues: 20});
    const i = r.issues;
    const msgs = [...new Set(i.messages.filter(m => m.severity <= 1).map(m => `${m.code}: ${m.message.slice(0,90)}`))].slice(0,4);
    console.log(`${path.basename(f)} E=${i.numErrors} W=${i.numWarnings} I=${i.numInfos} ${msgs.join(' | ')}`);
  }
})();
