// Build-time helper only; Node is not needed on a KVM appliance.
// Feed cargo metadata JSON on stdin. Never retain machine-local manifest paths.
import { readdir, mkdir, copyFile, writeFile, readFile } from 'node:fs/promises';
import { dirname, basename, join, resolve, relative, isAbsolute } from 'node:path';
let input = '';
for await (const chunk of process.stdin) input += chunk;
const metadata = JSON.parse(input);
const destination = 'dist/third-party-licenses';
await mkdir(destination, { recursive: true });
const records = [];
for (const pkg of metadata.packages) {
  if (!pkg.source) continue;
  if (!/^[a-zA-Z0-9_.+-]+$/.test(pkg.name) || !/^[a-zA-Z0-9_.+-]+$/.test(pkg.version)) {
    throw new Error('Unexpected package identifier');
  }
  const directory = dirname(pkg.manifest_path);
  const names = new Set((await readdir(directory, { withFileTypes: true }))
    .filter(entry => entry.isFile() && /^(licen[cs]e|copying|copyright|notice|unlicense|authors)([._-]|$)/i.test(entry.name))
    .map(entry => entry.name));
  const target = join(destination, `${pkg.name}-${pkg.version}`);
  await mkdir(target, { recursive: true });
  for (const name of names) await copyFile(join(directory, name), join(target, name));
  if (pkg.license_file && !names.has(basename(pkg.license_file))) {
    const licensePath = resolve(directory, pkg.license_file);
    const suffix = relative(directory, licensePath);
    if (suffix.startsWith('..') || isAbsolute(suffix)) throw new Error('License outside package');
    await copyFile(licensePath, join(target, basename(pkg.license_file)));
    names.add(basename(pkg.license_file));
  }
  let upstreamLicense;
  // These SDK crates omit their workspace-level license from the crate archive.
  // Fetch only the exact source revision recorded in the locked crate, never HEAD.
  if (!names.size && ['rmcp', 'rmcp-macros'].includes(pkg.name)) {
    const vcs = JSON.parse(await readFile(join(directory, '.cargo_vcs_info.json'), 'utf8'));
    if (!/^[a-f0-9]{40}$/.test(vcs.git.sha1)) throw new Error('Invalid SDK revision');
    upstreamLicense = `https://raw.githubusercontent.com/modelcontextprotocol/rust-sdk/${vcs.git.sha1}/LICENSE`;
    const response = await fetch(upstreamLicense, { redirect: 'error', signal: AbortSignal.timeout(30000) });
    if (!response.ok) throw new Error(`SDK license fetch failed: ${response.status}`);
    const text = await response.text();
    if (!text.includes('Apache License')) throw new Error('Unexpected SDK license');
    await writeFile(join(target, 'LICENSE'), text);
    names.add('LICENSE');
  }
  if (!names.size) throw new Error(`Missing license notice: ${pkg.name}-${pkg.version}`);
  records.push({ name: pkg.name, version: pkg.version, license: pkg.license,
    repository: pkg.repository, upstreamLicense, files: [...names].sort() });
}
records.sort((a,b) => `${a.name}@${a.version}`.localeCompare(`${b.name}@${b.version}`));
await writeFile(join(destination, 'index.json'), JSON.stringify(records,null,2)+'\n');
console.log(`Retained license records for ${records.length} dependency packages.`);
