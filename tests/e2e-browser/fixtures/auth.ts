import { createHmac, randomBytes } from "crypto";
import { writeFileSync } from "fs";
import { join } from "path";

/** The grant a token carries, in moq-auth's claim names. */
export interface Grant {
  root?: string;
  publish?: string[];
  subscribe?: string[];
}

/** A signing key, and the JWK file the relay verifies its tokens with. */
export interface AuthKey {
  /** The JWK file to pass as `--auth-key`. */
  path: string;
  /** Returns a JWT for `grant` that expires after `ttlSeconds`. */
  sign(grant: Grant, ttlSeconds?: number): string;
}

/**
 * Generates an HS256 key and writes it as a JWK to `dir/relay.jwk`.
 *
 * A symmetric key keeps this to Node's built-in `crypto`: the relay reads the
 * same JWK that `moq auth generate --algorithm HS256` writes, and the test
 * signs with the secret directly, so the e2e job needs neither the `moq` CLI
 * nor a JWT library.
 */
export function writeAuthKey(dir: string): AuthKey {
  const secret = randomBytes(32);
  const path = join(dir, "relay.jwk");
  const jwk = { kty: "oct", alg: "HS256", k: secret.toString("base64url") };
  writeFileSync(path, JSON.stringify(jwk), { mode: 0o600 });

  return {
    path,
    sign(grant, ttlSeconds = 600) {
      const now = Math.floor(Date.now() / 1000);
      const header = { alg: "HS256", typ: "JWT" };
      const claims = { ...grant, iat: now, exp: now + ttlSeconds };
      const encode = (value: object) =>
        Buffer.from(JSON.stringify(value)).toString("base64url");
      const body = `${encode(header)}.${encode(claims)}`;
      const signature = createHmac("sha256", secret).update(body).digest("base64url");
      return `${body}.${signature}`;
    },
  };
}
