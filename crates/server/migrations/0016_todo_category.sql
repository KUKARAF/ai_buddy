-- Scope + category for milestone TODO items.
--
-- Extends `step_todos` so a check-in can ADD new to-dos, not only complete
-- existing ones. `category` tags each item ('training'|'gear'|'logistics'|
-- 'prep'|'other'); a NULL (legacy) category is treated as 'training'. `goal_id`
-- denormalizes the owning goal (resolved via the step -> roadmap -> goal chain)
-- so non-training items (gear/logistics/prep) can be listed at the goal level
-- rather than under a single milestone. Legacy rows are back-filled: every
-- existing item is a training item of its step's goal.
ALTER TABLE step_todos ADD COLUMN goal_id TEXT;
ALTER TABLE step_todos ADD COLUMN category TEXT;  -- 'training'|'gear'|'logistics'|'prep'|'other'; NULL (legacy) treated as 'training'
UPDATE step_todos SET goal_id = (
  SELECT g.id FROM roadmap_steps s JOIN roadmaps r ON s.roadmap_id = r.id JOIN goals g ON r.goal_id = g.id
  WHERE s.id = step_todos.step_id
) WHERE goal_id IS NULL;
UPDATE step_todos SET category = 'training' WHERE category IS NULL;
CREATE INDEX idx_step_todos_goal ON step_todos(goal_id);
