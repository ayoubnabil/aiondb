Feature: OPTIONAL MATCH semantics often relied upon in Neo4j

  Scenario: optional match preserves bound row with null relationship target
    Given an empty graph
    And having executed:
      """
      MATCH (n) DETACH DELETE n;
      CREATE (:Person {name: 'Alice'});
      CREATE (:Person {name: 'Bob'})-[:KNOWS]->(:Person {name: 'Carol'});
      """
    When executing query:
      """
      MATCH (p:Person)
      OPTIONAL MATCH (p)-[:KNOWS]->(friend)
      WHERE p.name = 'Alice'
      RETURN p.name, friend.name
      """
    Then the result should be, in any order:
      | p.name  | friend.name |
      | 'Alice' | null        |

  Scenario: optional match returns bound and matched values when relationship exists
    Given an empty graph
    And having executed:
      """
      MATCH (n) DETACH DELETE n;
      CREATE (:Person {name: 'Bob'})-[:KNOWS]->(:Person {name: 'Carol'});
      """
    When executing query:
      """
      MATCH (p:Person)
      OPTIONAL MATCH (p)-[:KNOWS]->(friend)
      WHERE p.name = 'Bob'
      RETURN p.name, friend.name
      """
    Then the result should be, in any order:
      | p.name | friend.name |
      | 'Bob'  | 'Carol'     |

  Scenario: optional match with aggregation keeps unmatched row
    Given an empty graph
    And having executed:
      """
      MATCH (n) DETACH DELETE n;
      CREATE (:Person {name: 'Alice'});
      """
    When executing query:
      """
      MATCH (p:Person)
      OPTIONAL MATCH (p)-[:KNOWS]->(friend)
      RETURN p.name, count(friend) AS friend_count
      """
    Then the result should be, in any order:
      | p.name  | friend_count |
      | 'Alice' | 0            |
