/** Whether any of `fields` contains `query`, ignoring case and surrounding spaces. */
export function matchesQuery(query: string, ...fields: string[]): boolean {
  const wanted = query.trim().toLowerCase();
  return fields.some((field) => field.toLowerCase().includes(wanted));
}
