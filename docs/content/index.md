---
title: Home
order: 0
---

<div class="hero-stage">
<div class="hero-copy">
<p class="hero-kicker">PostgreSQL wire / ORM-compatible / SQL + graph + vector</p>
<h1>AionDB</h1>
<p class="lede">PostgreSQL tooling for applications that need relational records, graph relationships, and vector search in one Rust engine.</p>
<p class="hero-actions">
  <a class="button" href="/documentation/">Read the docs</a>
  <a class="button secondary" href="/presentation.html">Project overview</a>
</p>
</div>

<div class="hero-card">
<div class="hero-card-top">
<span class="hero-card-line"></span>
<span class="hero-card-label">One query surface</span>
</div>
<pre class="hero-query"><code><span class="tok-key">MATCH</span> (u:User {tenant_id: 100})-[:WROTE]-&gt;(d:Document)
      -[:CITES]-&gt;(ref:Document)
<span class="tok-key">WHERE</span> d.kind = 'runbook'
<span class="tok-key">RETURN</span> d.title, ref.title,
       <span class="tok-fn">l2_distance</span>(d.embedding, '[0.1,0.8,0.2]') AS dist,
       u.tenant_id
<span class="tok-key">ORDER BY</span> dist ASC
<span class="tok-key">LIMIT</span> 5;</code></pre>
<h2>Normal app stack.</h2>
<p>pgwire, ORMs, migrations, psql, pgAdmin.</p>
</div>
</div>

<div class="orm-band">
<div class="orm-band-head">
<p class="section-kicker">&gt;&gt; PostgreSQL ecosystem path</p>
<h2>Use normal ORMs and migration tools through pgwire.</h2>
</div>
<div class="orm-marquee" aria-label="Verified ORM and driver compatibility">
<div class="orm-track">
<div class="orm-card"><img src="/orm-prisma.svg" alt="" /><div><strong>Prisma</strong><span>verified ORM</span></div></div>
<div class="orm-card"><img src="/orm-typeorm.svg" alt="" /><div><strong>TypeORM</strong><span>verified ORM</span></div></div>
<div class="orm-card"><img src="/orm-sequelize.svg" alt="" /><div><strong>Sequelize</strong><span>verified ORM</span></div></div>
<div class="orm-card"><img src="/orm-knexdotjs.svg" alt="" /><div><strong>Knex.js</strong><span>verified ORM</span></div></div>
<div class="orm-card"><img src="/orm-sqlalchemy.svg" alt="" /><div><strong>SQLAlchemy</strong><span>verified ORM</span></div></div>
<div class="orm-card"><span aria-hidden="true">dj</span><div><strong>Django</strong><span>verified ORM</span></div></div>
<div class="orm-card"><span aria-hidden="true">{}</span><div><strong>Alembic</strong><span>verified migrations</span></div></div>
<div class="orm-card"><span aria-hidden="true">psql</span><div><strong>psycopg</strong><span>verified driver</span></div></div>
<div class="orm-card"><img src="/orm-prisma.svg" alt="" /><div><strong>Prisma</strong><span>verified ORM</span></div></div>
<div class="orm-card"><img src="/orm-typeorm.svg" alt="" /><div><strong>TypeORM</strong><span>verified ORM</span></div></div>
<div class="orm-card"><img src="/orm-sequelize.svg" alt="" /><div><strong>Sequelize</strong><span>verified ORM</span></div></div>
<div class="orm-card"><img src="/orm-knexdotjs.svg" alt="" /><div><strong>Knex.js</strong><span>verified ORM</span></div></div>
<div class="orm-card"><img src="/orm-sqlalchemy.svg" alt="" /><div><strong>SQLAlchemy</strong><span>verified ORM</span></div></div>
<div class="orm-card"><span aria-hidden="true">dj</span><div><strong>Django</strong><span>verified ORM</span></div></div>
<div class="orm-card"><span aria-hidden="true">{}</span><div><strong>Alembic</strong><span>verified migrations</span></div></div>
<div class="orm-card"><span aria-hidden="true">psql</span><div><strong>psycopg</strong><span>verified driver</span></div></div>
</div>
</div>
<p class="orm-note">The official compatibility gate now includes Django migrations, NOT NULL enforcement, composite UNIQUE checks, rollback, and schema introspection.</p>
</div>

<div class="home-section">
<p class="section-kicker">&gt;&gt; Data model</p>
<h2>Built for application data that outgrows one shape.</h2>
<div class="feature-grid">
<div class="feature-card"><h3>Relational state</h3><p>Tables remain the source of truth, queried through SQL and PostgreSQL-compatible clients.</p></div>
<div class="feature-card"><h3>Connected data</h3><p>Graph labels and Cypher-style patterns are available over ordinary application records.</p></div>
<div class="feature-card"><h3>Semantic retrieval</h3><p>Vector columns and distance functions live beside metadata instead of in a detached service.</p></div>
<div class="feature-card"><h3>Local integration</h3><p>The engine can run as a server over pgwire or through the embedded Rust API.</p></div>
</div>
</div>

<div class="home-section">
<div class="performance-position">
<p class="section-kicker">&gt;&gt; Measured, not universal claims</p>
<h2>Performance position</h2>
<p>AionDB is not presented as universally faster than mature single-purpose systems. The useful claim is narrower: it already performs competitively on many measured SQL workloads, is often materially faster than SurrealDB on the measured SQL, graph, and hybrid workloads in the public harnesses, and publishes the raw benchmark snapshots used by the docs.</p>
<p>See <a href="/documentation/evaluate/benchmarks.html">Benchmarks</a> and <a href="/documentation/evaluate/benchmark-results.html">Benchmark Results</a> for exact measured cases.</p>
</div>
</div>

<div class="home-section">
<p class="section-kicker">&gt;&gt; Alpha boundaries</p>
<h2>What AionDB does not claim.</h2>
<div class="claim-grid">
<div class="claim-card"><h3>PostgreSQL replacement</h3><p>No. PostgreSQL remains the compatibility and operational maturity reference.</p></div>
<div class="claim-card"><h3>Production-ready cluster</h3><p>No. Distributed and HA modules are internal or experimental in v0.1.</p></div>
<div class="claim-card"><h3>Every ORM works</h3><p>No. Many harnesses pass now, but every app still needs feature-by-feature validation.</p></div>
<div class="claim-card"><h3>Best on every benchmark</h3><p>No. The project publishes reproducible benchmark snapshots instead of universal speed claims.</p></div>
</div>
</div>
