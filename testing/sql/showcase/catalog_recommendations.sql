-- Showcase dataset 2: catalog + recommendations for e-commerce / media

CREATE TABLE products (
    id INT NOT NULL,
    sku TEXT,
    title TEXT,
    category TEXT,
    price INT,
    embedding VECTOR(2)
);

CREATE TABLE brands (
    id INT NOT NULL,
    name TEXT
);

CREATE TABLE product_brand_edges (
    source_id INT NOT NULL,
    target_id INT NOT NULL
);

CREATE TABLE product_links (
    source_id INT NOT NULL,
    target_id INT NOT NULL,
    relation TEXT
);

CREATE TABLE query_vectors (
    id INT NOT NULL,
    label TEXT,
    embedding VECTOR(2)
);

CREATE NODE LABEL product ON products;
CREATE NODE LABEL brand ON brands;
CREATE EDGE LABEL product_brand ON product_brand_edges SOURCE product TARGET brand;
CREATE EDGE LABEL related_product ON product_links SOURCE product TARGET product;

INSERT INTO products VALUES
    (1, 'sku-headphones', 'Noise Cancelling Headphones', 'audio', 300, '[1.0,0.0]'),
    (2, 'sku-case', 'Travel Carry Case', 'accessory', 40, '[0.95,0.05]'),
    (3, 'sku-earbuds', 'Wireless Earbuds', 'audio', 180, '[0.8,0.2]'),
    (4, 'sku-turntable', 'Vinyl Record Player', 'audio', 220, '[0.0,1.0]'),
    (5, 'sku-jazz-pass', 'Jazz Streaming Pass', 'media', 15, '[0.1,0.9]'),
    (6, 'sku-stand', 'Speaker Stand', 'accessory', 80, '[0.7,0.3]'),
    (7, 'sku-amp', 'Hi-Fi Amplifier', 'audio', 240, '[0.2,0.8]');

INSERT INTO brands VALUES
    (10, 'Acme Audio'),
    (20, 'Retro Sound');

INSERT INTO product_brand_edges VALUES
    (1, 10),
    (2, 10),
    (3, 10),
    (6, 10),
    (4, 20),
    (5, 20),
    (7, 20);

INSERT INTO product_links VALUES
    (1, 2, 'bundle'),
    (1, 3, 'similar'),
    (1, 6, 'bundle'),
    (4, 5, 'pairs_with'),
    (4, 7, 'pairs_with'),
    (3, 2, 'accessory');

INSERT INTO query_vectors VALUES
    (1, 'portable_audio', '[0.85,0.15]'),
    (2, 'living_room_listening', '[0.1,0.9]');
