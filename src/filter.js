// Profanity / slur filter. Checked case-insensitively against pilot names
// before they're stored or broadcast. List covers the most common hate-speech
// terms across several languages; leet-speak variants are normalised first.

const BLOCKED = [
  'nigger','nigga','nigg3r','n1gger','n1gga','niggah',
  'chink','spic','spick','kike','gook','wetback','beaner',
  'towelhead','raghead','sandnigger','coon','jigaboo','sambo',
  'zipperhead','slope','nip','wog','paki','cracker',
  'faggot','fagot','fag','dyke','tranny',
  'retard','retarded','spaz',
  'whore','cunt','slut','twat',
  'chigger','groid','zipperhead',
];

// Collapse leet-speak before matching.
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
