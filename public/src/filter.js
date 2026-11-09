const BLOCKED = [
  'nigger', 'nigga', 'niggah',
  'chink', 'spic', 'spick', 'kike', 'gook', 'wetback', 'beaner',
  'towelhead', 'raghead', 'sandnigger', 'coon', 'jigaboo', 'sambo',
  'zipperhead', 'slope', 'nip', 'wog', 'paki', 'cracker',
  'faggot', 'fagot', 'fag', 'dyke', 'tranny',
  'retard', 'retarded', 'spaz',
  'whore', 'cunt', 'slut', 'twat',
  'chigger', 'groid',
];
function normalise(text) {
  return text.toLowerCase()
    .replace(/0/g, 'o')
    .replace(/1/g, 'i')
    .replace(/3/g, 'e')
    .replace(/4/g, 'a')
    .replace(/5/g, 's')
    .replace(/[^a-z]/g, '');
}
export function containsProfanity(text) {
  const n = normalise(text);
  return BLOCKED.some(w => n.includes(w));
}