export function canonical(value) {
  if (Array.isArray(value)) return value.map(canonical);
  if (value && typeof value === "object") return Object.fromEntries(Object.keys(value).sort().map(k => [k, canonical(value[k])]));
  return value;
}
const terms = (name, pricing, maximum = null) => ({ name, pricing, monthly_hours: { minimum: null, maximum }, effective_on: "1900-01-01" });
export const operations = [
  { AddClient: { name: "golden" } },
  { AddProject: { client: "1", name: "hourly", zone: "America/New_York",
    terms: terms("hourly", { Hourly: { amount_micros: "9223372036854775807", currency: "EUR" } }) } },
  { AddProject: { client: "1", name: "retainer", zone: "Europe/Berlin",
    terms: terms("retainer", { Retainer: { fee: { amount_micros: "9223372036854775807", currency: "EUR" }, included_hours: "20", hourly_rate: null } }, "20") } },
  { Log: { agreement: "1", scope: "spring DST", started_at: "2024-03-10T06:30:00Z", ended_at: "2024-03-10T08:30:00Z" } },
  { Log: { agreement: "2", scope: "fall DST", started_at: "2024-10-27T00:30:00Z", ended_at: "2024-10-27T02:30:00Z" } },
  { Log: { agreement: "1", scope: "long calendar span", started_at: "1900-01-01T05:00:00Z", ended_at: "2020-01-01T05:00:00Z" } },
].map(canonical);
export const queries = [
  { Overview: { zone: "Europe/Berlin" } },
  { Entries: { agreement: "1", limit: 100, cursor: null } },
  { RecentScopes: { project: "1", limit: 20 } },
].map(canonical);
export function normalize(value) {
  if (Array.isArray(value)) return value.map(normalize);
  if (value && typeof value === "object") return Object.fromEntries(Object.entries(value)
    .filter(([key]) => !["authority", "calculated_at"].includes(key)).map(([key, v]) => [key, normalize(v)]));
  return value;
}
