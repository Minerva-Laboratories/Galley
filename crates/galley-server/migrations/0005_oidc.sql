-- Single sign-on. One account per provider subject, found on every sign-in.
CREATE UNIQUE INDEX users_oidc_sub ON users (oidc_sub) WHERE oidc_sub IS NOT NULL;
