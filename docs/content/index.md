---
title: Home
seo_title: AionDB - multimodal database for SQL, graph, and vector search
description: AionDB is a PostgreSQL-compatible multimodal database in Rust that combines relational records, graph relationships, and vector search in one engine.
order: 0
---

<div class="hero-stage">
<div class="hero-copy">
<p class="hero-kicker">PostgreSQL wire / ORM-compatible / SQL + graph + vector</p>
<h1>AionDB</h1>
<p class="lede">AionDB v0.3 brings pgvector-style SQL, HNSW, IVF-flat, Qdrant-style filters, graph relationships, and relational records into one Rust engine.</p>
<p class="hero-actions">
  <a class="button" href="/documentation/project/whats-new-v0-3.html">v0.3 vector update</a>
  <a class="button secondary" href="/documentation/evaluate/v0-3-vector-performance.html">Vector benchmarks</a>
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

<div class="home-section">
<p class="section-kicker">&gt;&gt; v0.3 vector update</p>
<h2>Vector search runs inside the SQL engine.</h2>
<div class="feature-grid">
<div class="feature-card"><h3>HNSW</h3><p>Raw vector search reaches 0.996 recall@10 in the default v0.3 vector benchmark.</p></div>
<div class="feature-card"><h3>HNSW PQ</h3><p>Product-quantized search keeps 0.994 recall@10 while exercising compressed candidate search and exact rescoring.</p></div>
<div class="feature-card"><h3>IVF-flat</h3><p>The default dataset builds in about 416-418 ms, with nprobe=32 reaching 0.863 recall@10 around 2.57 ms mean latency.</p></div>
<div class="feature-card"><h3>Filtered retrieval</h3><p>Qdrant-style JSON filters bring tenant, permission, metadata, payload, and nested JSONB controls into vector helper calls.</p></div>
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
<p class="section-kicker">&gt;&gt; Benchmarked product direction</p>
<h2>Performance position</h2>
<p>v0.3 makes vector performance visible: high-recall HNSW, fast-building IVF-flat, exact recall reference, and published commands. The benchmark path sits beside the existing SQL, graph, and hybrid harnesses so product claims stay tied to reproducible runs.</p>
<p>See <a href="/documentation/evaluate/v0-3-vector-performance.html">v0.3 Vector Performance</a>, <a href="/documentation/evaluate/benchmarks.html">Benchmarks</a>, and <a href="/documentation/evaluate/benchmark-results.html">Benchmark Results</a>.</p>
</div>
</div>
